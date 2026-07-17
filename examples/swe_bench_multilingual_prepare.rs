#[path = "support/swe_bench_multilingual.rs"]
mod swe_bench_multilingual;

use std::{collections::BTreeSet, error::Error, path::PathBuf};

use clap::Parser;
use leantoken::tokens::Tokenizer;
use swe_bench_multilingual::{Language, PrepareConfig, prepare};

#[derive(Debug, Parser)]
#[command(about = "Prepare a balanced, label-sealed SWE-bench Multilingual development set")]
struct Args {
    /// Canonical JSONL export of the pinned dataset artifact.
    #[arg(long)]
    dataset: PathBuf,
    /// Pinned source Parquet artifact; it is hashed but never parsed or copied.
    #[arg(long)]
    source_artifact: PathBuf,
    /// Exact 40-character Hugging Face dataset revision.
    #[arg(long)]
    source_revision: String,
    /// Revision-bound HTTPS URL for the source Parquet artifact.
    #[arg(long)]
    source_url: String,
    /// Stable selection seed committed before candidate evaluation.
    #[arg(long, default_value = "leantoken-sbml-development-v1")]
    seed: String,
    /// Exact 40-character Git revision used to build this harness.
    #[arg(long)]
    harness_revision: String,
    /// Comma-separated target languages; empty means all nine dataset languages.
    #[arg(long = "language", value_delimiter = ',')]
    languages: Vec<Language>,
    /// Selected tasks in each language stratum.
    #[arg(long, default_value_t = 6)]
    tasks_per_language: usize,
    /// Minimum behavioral/non-exact tasks in each language stratum.
    #[arg(long, default_value_t = 3)]
    non_exact_per_language: usize,
    /// Maximum selected tasks from one repository.
    #[arg(long, default_value_t = 5)]
    max_tasks_per_repository: usize,
    /// Frozen LeanToken source-token budget for every task.
    #[arg(long, default_value_t = 2_000)]
    source_token_budget: usize,
    /// Exact tokenizer used to enforce every source-token budget.
    #[arg(long, value_enum, default_value_t = Tokenizer::default())]
    tokenizer: Tokenizer,
    /// Optional audited repository-license JSON array.
    #[arg(long)]
    repository_license_map: Option<PathBuf>,
    /// Reject preparation unless every selected repository has an audited license entry.
    #[arg(long)]
    require_license_audit: bool,
    /// Public task records without evaluator labels.
    #[arg(long)]
    tasks_output: PathBuf,
    /// Private evaluator labels. The file is created owner-readable only on Unix.
    #[arg(long)]
    labels_output: PathBuf,
    /// Publishable aggregate receipt containing artifact commitments and limitations.
    #[arg(long)]
    receipt_output: PathBuf,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    let harness_binary = std::env::current_exe()?;
    let languages = if args.languages.is_empty() {
        Language::ALL.into_iter().collect::<BTreeSet<_>>()
    } else {
        args.languages.into_iter().collect()
    };
    let receipt = prepare(&PrepareConfig {
        dataset_jsonl: &args.dataset,
        source_artifact: &args.source_artifact,
        source_revision: &args.source_revision,
        source_url: &args.source_url,
        seed: &args.seed,
        harness_revision: &args.harness_revision,
        harness_binary: &harness_binary,
        languages,
        tasks_per_language: args.tasks_per_language,
        non_exact_per_language: args.non_exact_per_language,
        max_tasks_per_repository: args.max_tasks_per_repository,
        source_token_budget: args.source_token_budget,
        tokenizer: args.tokenizer,
        repository_license_map: args.repository_license_map.as_deref(),
        require_license_audit: args.require_license_audit,
        tasks_output: &args.tasks_output,
        labels_output: &args.labels_output,
        receipt_output: &args.receipt_output,
    })?;
    println!("{}", serde_json::to_string_pretty(&receipt)?);
    Ok(())
}
