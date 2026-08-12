use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use clap::Parser;
use leantoken::{
    Config, ReadDeltaFallback, ReadDeltaOutcome, ReadResponse, ReadStatus, WorktreeReadRequest,
    services::Services,
};
use serde::Serialize;

#[derive(Debug, Parser)]
#[command(about = "Measure opt-in exact-read delta decisions on bounded edit workflows")]
struct Args {
    #[arg(long, default_value = "target/read_delta_benchmark_report.json")]
    output: PathBuf,
}

#[derive(Clone, Copy)]
enum Target {
    WholeFile,
    Symbol(&'static str),
}

struct Case {
    name: &'static str,
    source: String,
    changed: String,
    target: Target,
    capture_base: bool,
    reindex_after_edit: bool,
    expected_outcome: ReadDeltaOutcome,
    expected_fallback: Option<ReadDeltaFallback>,
    expected_removed: Option<&'static str>,
    expected_added: Option<&'static str>,
}

#[derive(Serialize)]
struct Report {
    schema_version: u32,
    leantoken_version: &'static str,
    tokenizer: &'static str,
    harness_revision: Option<String>,
    harness_worktree_dirty: Option<bool>,
    aggregate: Aggregate,
    cases: Vec<CaseReport>,
    limitations: Vec<&'static str>,
}

#[derive(Default, Serialize)]
struct Aggregate {
    case_count: usize,
    delta_count: usize,
    full_fallback_count: usize,
    full_tokens: usize,
    returned_source_tokens: usize,
    avoided_source_tokens: usize,
    source_savings_fraction: f64,
    full_response_tokens: usize,
    returned_response_tokens: usize,
    response_token_delta: i64,
    response_savings_fraction: f64,
}

#[derive(Serialize)]
struct CaseReport {
    name: &'static str,
    status: ReadStatus,
    outcome: ReadDeltaOutcome,
    fallback_reason: Option<ReadDeltaFallback>,
    full_tokens: usize,
    returned_source_tokens: usize,
    avoided_source_tokens: usize,
    full_response_tokens: usize,
    returned_response_tokens: usize,
    response_token_delta: i64,
    base_generation: Option<u64>,
    head_generation: u64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    let cases = cases();
    let mut reports = Vec::with_capacity(cases.len());
    for case in cases {
        reports.push(run_case(case).await?);
    }
    let mut aggregate = Aggregate::default();
    for report in &reports {
        aggregate.case_count += 1;
        aggregate.delta_count += usize::from(report.outcome == ReadDeltaOutcome::Delta);
        aggregate.full_fallback_count += usize::from(
            report.outcome == ReadDeltaOutcome::Full && report.fallback_reason.is_some(),
        );
        aggregate.full_tokens += report.full_tokens;
        aggregate.returned_source_tokens += report.returned_source_tokens;
        aggregate.avoided_source_tokens += report.avoided_source_tokens;
        aggregate.full_response_tokens += report.full_response_tokens;
        aggregate.returned_response_tokens += report.returned_response_tokens;
        aggregate.response_token_delta += report.response_token_delta;
    }
    aggregate.source_savings_fraction = if aggregate.full_tokens == 0 {
        0.0
    } else {
        aggregate.avoided_source_tokens as f64 / aggregate.full_tokens as f64
    };
    aggregate.response_savings_fraction = if aggregate.full_response_tokens == 0 {
        0.0
    } else {
        1.0 - aggregate.returned_response_tokens as f64 / aggregate.full_response_tokens as f64
    };
    let repository = PathBuf::from(env!("LEANTOKEN_REPOSITORY_ROOT"));
    let harness_revision = git_output(&repository, &["rev-parse", "HEAD"]);
    let harness_worktree_dirty = git_output(
        &repository,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )
    .map(|status| !status.trim().is_empty());
    let report = Report {
        schema_version: 1,
        leantoken_version: env!("LEANTOKEN_PRODUCT_VERSION"),
        tokenizer: leantoken::tokens::Tokenizer::default().name(),
        harness_revision,
        harness_worktree_dirty,
        aggregate,
        cases: reports,
        limitations: vec![
            "This protocol benchmark uses deterministic edit fixtures and does not measure model task success.",
            "Source-token savings exclude the request and response metadata retained for provenance.",
            "The feature remains opt-in and does not apply to ranked context or continuation pages.",
        ],
    };
    let json = serde_json::to_string_pretty(&report)?;
    if let Some(parent) = args
        .output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    fs::write(&args.output, format!("{json}\n"))?;
    println!("{json}");
    Ok(())
}

async fn run_case(case: Case) -> Result<CaseReport, Box<dyn Error>> {
    let root = tempfile::tempdir()?;
    fs::write(root.path().join("fixture.rs"), &case.source)?;
    let services = Services::open(Config::discover(
        root.path(),
        Some(root.path().join("index.sqlite")),
    )?)?;
    services.refresh(leantoken::IndexingMode::Rebuild).await?;
    let first = services
        .read_worktree(request(case.target, None, case.capture_base))
        .await?;
    fs::write(root.path().join("fixture.rs"), &case.changed)?;
    if case.reindex_after_edit {
        services.refresh(leantoken::IndexingMode::Reconcile).await?;
    }
    let base_hash = first.content_hash;
    let changed = services
        .read_worktree(request(case.target, Some(base_hash.clone()), true))
        .await?;
    validate_case(&case, &changed)?;
    let full = services
        .read_worktree(request(case.target, Some(base_hash), false))
        .await?;
    if full.status != ReadStatus::Content || full.meta.source_tokens == 0 {
        return Err(format!("{} full-response control was not content", case.name).into());
    }
    if changed.status == ReadStatus::Delta
        && serialized_tokens(&changed)? >= serialized_tokens(&full)?
    {
        return Err(format!(
            "{} delta response was not strictly cheaper than full JSON",
            case.name
        )
        .into());
    }
    let receipt = changed
        .delta_receipt
        .as_ref()
        .ok_or("delta benchmark response omitted its receipt")?;
    Ok(CaseReport {
        name: case.name,
        status: changed.status.clone(),
        outcome: receipt.outcome.clone(),
        fallback_reason: receipt.fallback_reason.clone(),
        full_tokens: receipt.full_tokens,
        returned_source_tokens: changed.meta.source_tokens,
        avoided_source_tokens: receipt.avoided_tokens,
        full_response_tokens: serialized_tokens(&full)?,
        returned_response_tokens: serialized_tokens(&changed)?,
        response_token_delta: i64::try_from(serialized_tokens(&changed)?)
            .unwrap_or(i64::MAX)
            .saturating_sub(i64::try_from(serialized_tokens(&full)?).unwrap_or(i64::MAX)),
        base_generation: receipt.base_generation,
        head_generation: receipt.head_generation,
    })
}

fn serialized_tokens(response: &ReadResponse) -> Result<usize, serde_json::Error> {
    Ok(leantoken::tokens::Tokenizer::default().count(&serde_json::to_string(response)?))
}

fn validate_case(case: &Case, response: &ReadResponse) -> Result<(), Box<dyn Error>> {
    let receipt = response
        .delta_receipt
        .as_ref()
        .ok_or("delta benchmark response omitted its receipt")?;
    if receipt.outcome != case.expected_outcome {
        return Err(format!(
            "{} returned {:?}, expected {:?}",
            case.name, receipt.outcome, case.expected_outcome
        )
        .into());
    }
    if receipt.fallback_reason != case.expected_fallback {
        return Err(format!(
            "{} fallback was {:?}, expected {:?}",
            case.name, receipt.fallback_reason, case.expected_fallback
        )
        .into());
    }
    if receipt.outcome == ReadDeltaOutcome::Delta {
        let delta = response.delta.as_deref().ok_or("delta content missing")?;
        if receipt.delta_tokens != Some(response.meta.source_tokens)
            || receipt.avoided_tokens == 0
            || receipt.full_tokens <= response.meta.source_tokens
        {
            return Err(format!("{} did not record strict token savings", case.name).into());
        }
        for expected in [case.expected_removed, case.expected_added]
            .into_iter()
            .flatten()
        {
            if !delta.contains(expected) {
                return Err(format!("{} delta omitted {expected}", case.name).into());
            }
        }
    } else {
        let content = response
            .content
            .as_deref()
            .ok_or("fallback content missing")?;
        let content_matches = match case.target {
            Target::WholeFile => content == case.changed,
            Target::Symbol(_) => !content.is_empty() && case.changed.contains(content),
        };
        if !content_matches {
            return Err(
                format!("{} fallback did not return full current target", case.name).into(),
            );
        }
    }
    Ok(())
}

fn request(target: Target, expected_hash: Option<String>, delta: bool) -> WorktreeReadRequest {
    let symbol = match target {
        Target::WholeFile => None,
        Target::Symbol(symbol) => Some(symbol.to_owned()),
    };
    WorktreeReadRequest {
        path: "fixture.rs".into(),
        start_line: None,
        end_line: None,
        symbol,
        heading: None,
        heading_occurrence: None,
        continuation_cursor: None,
        max_tokens: Some(32_000),
        expected_hash,
        delta,
        delta_base_artifact_id: None,
        receipt_id: None,
        policy: leantoken::model::ReadPolicy::default(),
    }
}

fn cases() -> Vec<Case> {
    let real_source = include_str!("../src/services/read/mod.rs").to_owned();
    let real_changed = real_source.replacen(
        "const MIN_CONTEXT_RANGE_LINES: usize = 12;",
        "const MIN_CONTEXT_RANGE_LINES: usize = 16;",
        1,
    );
    assert_ne!(
        real_source, real_changed,
        "real-source edit fixture drifted"
    );
    vec![
        Case {
            name: "real_source_line_edit",
            source: real_source,
            changed: real_changed,
            target: Target::WholeFile,
            capture_base: true,
            reindex_after_edit: false,
            expected_outcome: ReadDeltaOutcome::Delta,
            expected_fallback: None,
            expected_removed: Some("-const MIN_CONTEXT_RANGE_LINES: usize = 12;"),
            expected_added: Some("+const MIN_CONTEXT_RANGE_LINES: usize = 16;"),
        },
        Case {
            name: "small_uneconomic_edit",
            source: "fn alpha() {}\n".into(),
            changed: "fn beta() {}\n".into(),
            target: Target::WholeFile,
            capture_base: true,
            reindex_after_edit: false,
            expected_outcome: ReadDeltaOutcome::Full,
            expected_fallback: Some(ReadDeltaFallback::DeltaNotSmaller),
            expected_removed: None,
            expected_added: None,
        },
        Case {
            name: "moved_symbol",
            source: "fn target() {\n    old_behavior();\n}\n".into(),
            changed: "\nfn target() {\n    new_behavior();\n}\n".into(),
            target: Target::Symbol("target"),
            capture_base: true,
            reindex_after_edit: true,
            expected_outcome: ReadDeltaOutcome::Full,
            expected_fallback: Some(ReadDeltaFallback::TargetChanged),
            expected_removed: None,
            expected_added: None,
        },
        Case {
            name: "missing_base",
            source: "fn before() {\n    unchanged();\n}\n".into(),
            changed: "fn after() {\n    unchanged();\n}\n".into(),
            target: Target::WholeFile,
            capture_base: false,
            reindex_after_edit: false,
            expected_outcome: ReadDeltaOutcome::Full,
            expected_fallback: Some(ReadDeltaFallback::BaseUnavailable),
            expected_removed: None,
            expected_added: None,
        },
    ]
}

fn git_output(root: &std::path::Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
