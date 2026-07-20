use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use clap::Parser;
use serde::{Deserialize, Serialize};

type AnyResult<T> = Result<T, Box<dyn Error>>;

const PLATFORMS: &[&str] = &["ubuntu-latest", "macos-latest", "windows-latest"];

#[derive(Debug, Parser)]
#[command(about = "Aggregate and decide the pinned monorepo reconciliation matrix")]
struct Args {
    /// Frozen experiment and corpus manifest.
    #[arg(long)]
    manifest: PathBuf,
    /// Directory containing downloaded per-pair artifact directories.
    #[arg(long)]
    artifacts: PathBuf,
    /// Aggregate JSON report destination.
    #[arg(long)]
    output: PathBuf,
}

#[derive(Debug, Deserialize)]
struct Manifest {
    schema_version: u32,
    experiment: String,
    profile: ProfilePolicy,
    adoption_threshold: AdoptionThreshold,
    corpora: Vec<Corpus>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ProfilePolicy {
    iterations: usize,
    read_samples: usize,
    hot_set: usize,
    watcher_debounce_ms: u64,
}

#[derive(Debug, Deserialize, Serialize)]
struct AdoptionThreshold {
    full_fallback_p95_ms: f64,
    discovery_and_hash_plan_share_percent: f64,
    minimum_material_corpus_platform_pairs: usize,
    rule: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct Corpus {
    id: String,
    repository: String,
    revision: String,
}

#[derive(Debug, Deserialize)]
struct Profile {
    schema_version: u32,
    leantoken_git_revision: Option<String>,
    leantoken_worktree_dirty: Option<bool>,
    host_os: String,
    host_arch: String,
    release_build: bool,
    corpus: ProfileCorpus,
    initial_index: InitialIndex,
    full_noop: IndexMeasurement,
    full_noop_phases: PhaseMeasurement,
    targeted_changed: IndexMeasurement,
    create_delta: IndexMeasurement,
    delete_targeted: IndexMeasurement,
    rename_delta: IndexMeasurement,
    directory_rename_delta: Option<DirectoryRenameMeasurement>,
    ignore_visibility_delta: IndexMeasurement,
    watcher_modify_delivery: WatcherDelivery,
    final_storage_footprint: StorageFootprint,
}

#[derive(Debug, Deserialize)]
struct ProfileCorpus {
    source_repository: Option<String>,
    revision: Option<String>,
    files: usize,
    total_bytes: u64,
    max_directory_depth: usize,
    indexing_iterations: usize,
    read_samples: usize,
    hot_set_files: usize,
}

#[derive(Debug, Deserialize)]
struct InitialIndex {
    elapsed_ms: f64,
    storage_footprint: StorageFootprint,
}

#[derive(Debug, Deserialize)]
struct IndexMeasurement {
    timing: TimingStats,
}

#[derive(Debug, Deserialize)]
struct DirectoryRenameMeasurement {
    affected_files: usize,
    indexing: IndexMeasurement,
}

#[derive(Debug, Deserialize)]
struct PhaseMeasurement {
    total: TimingStats,
    discovery: TimingStats,
    hash_and_plan: TimingStats,
    preparation: TimingStats,
    insertion: TimingStats,
    publication: TimingStats,
}

#[derive(Debug, Deserialize)]
struct WatcherDelivery {
    configured_debounce_ms: u64,
    timing: TimingStats,
    changed_messages: usize,
    full_reconciliation_messages: usize,
}

#[derive(Debug, Deserialize, Serialize)]
struct StorageFootprint {
    database_bytes: u64,
    wal_bytes: u64,
    shm_bytes: u64,
    total_bytes: u64,
}

#[derive(Debug, Deserialize)]
struct TimingStats {
    samples: usize,
    p50_us: f64,
    p95_us: f64,
}

#[derive(Debug, Deserialize)]
struct PeakRss {
    peak_rss_bytes: u64,
    source: String,
}

#[derive(Debug, Serialize)]
struct AggregateReport {
    schema_version: u32,
    experiment: String,
    manifest_schema_version: u32,
    leantoken_git_revision: String,
    profile: ProfilePolicy,
    adoption_threshold: AdoptionThreshold,
    expected_pairs: usize,
    material_pairs: usize,
    prototype_incremental_machinery: bool,
    decision: String,
    results: Vec<PairResult>,
    limitations: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct PairResult {
    platform: String,
    host_os: String,
    host_arch: String,
    corpus_id: String,
    corpus_repository: String,
    corpus_revision: String,
    files: usize,
    total_bytes: u64,
    max_directory_depth: usize,
    peak_rss_bytes: u64,
    peak_rss_source: String,
    initial_index_ms: f64,
    initial_storage_footprint: StorageFootprint,
    final_storage_footprint: StorageFootprint,
    full_noop_p50_ms: f64,
    full_noop_p95_ms: f64,
    discovery_p50_ms: f64,
    discovery_p95_ms: f64,
    hash_and_plan_p50_ms: f64,
    hash_and_plan_p95_ms: f64,
    preparation_p95_ms: f64,
    insertion_p95_ms: f64,
    publication_p95_ms: f64,
    dominant_full_noop_phase: &'static str,
    discovery_and_hash_plan_share_percent: f64,
    targeted_modify_p50_ms: f64,
    targeted_modify_p95_ms: f64,
    create_p50_ms: f64,
    create_p95_ms: f64,
    delete_p50_ms: f64,
    delete_p95_ms: f64,
    file_rename_p50_ms: f64,
    file_rename_p95_ms: f64,
    directory_rename_affected_files: usize,
    directory_rename_p50_ms: f64,
    directory_rename_p95_ms: f64,
    semantic_ignore_p50_ms: f64,
    semantic_ignore_p95_ms: f64,
    watcher_delivery_p50_ms: f64,
    watcher_delivery_p95_ms: f64,
    watcher_changed_messages: usize,
    watcher_full_reconciliation_messages: usize,
    material_full_scan: bool,
}

fn main() -> AnyResult<()> {
    let args = Args::parse();
    let manifest: Manifest = serde_json::from_slice(&fs::read(&args.manifest)?)?;
    let report = aggregate(&manifest, &args.artifacts)?;
    let json = serde_json::to_string_pretty(&report)?;
    if let Some(parent) = args.output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&args.output, format!("{json}\n"))?;
    println!("{json}");
    Ok(())
}

fn aggregate(manifest: &Manifest, artifacts: &Path) -> AnyResult<AggregateReport> {
    if manifest.schema_version != 1 {
        return Err(invalid_data("unsupported monorepo manifest schema"));
    }
    if manifest.corpora.is_empty() || manifest.profile.iterations == 0 {
        return Err(invalid_data(
            "manifest requires corpora and positive iterations",
        ));
    }
    let mut results = Vec::with_capacity(manifest.corpora.len() * PLATFORMS.len());
    let mut runtime_revision = None;
    for corpus in &manifest.corpora {
        for platform in PLATFORMS {
            let directory = artifacts.join(format!("monorepo-profile-{platform}-{}", corpus.id));
            let profile: Profile = read_json(&directory.join("profile.json"))?;
            let peak: PeakRss = read_json(&directory.join("peak-rss.json"))?;
            validate_profile(manifest, corpus, &profile, &mut runtime_revision)?;
            results.push(pair_result(manifest, corpus, platform, profile, peak)?);
        }
    }
    let material_pairs = results
        .iter()
        .filter(|result| result.material_full_scan)
        .count();
    let prototype_incremental_machinery = material_pairs
        >= manifest
            .adoption_threshold
            .minimum_material_corpus_platform_pairs;
    let decision = if prototype_incremental_machinery {
        format!(
            "prototype eligible: {material_pairs} corpus/platform pairs met the frozen materiality threshold"
        )
    } else {
        format!(
            "no-go: {material_pairs} corpus/platform pairs met the frozen materiality threshold; retain targeted reconciliation with bounded full fallback"
        )
    };
    Ok(AggregateReport {
        schema_version: 1,
        experiment: manifest.experiment.clone(),
        manifest_schema_version: manifest.schema_version,
        leantoken_git_revision: runtime_revision
            .ok_or_else(|| invalid_data("matrix contained no runtime revision"))?,
        profile: ProfilePolicy {
            iterations: manifest.profile.iterations,
            read_samples: manifest.profile.read_samples,
            hot_set: manifest.profile.hot_set,
            watcher_debounce_ms: manifest.profile.watcher_debounce_ms,
        },
        adoption_threshold: AdoptionThreshold {
            full_fallback_p95_ms: manifest.adoption_threshold.full_fallback_p95_ms,
            discovery_and_hash_plan_share_percent: manifest
                .adoption_threshold
                .discovery_and_hash_plan_share_percent,
            minimum_material_corpus_platform_pairs: manifest
                .adoption_threshold
                .minimum_material_corpus_platform_pairs,
            rule: manifest.adoption_threshold.rule.clone(),
        },
        expected_pairs: results.len(),
        material_pairs,
        prototype_incremental_machinery,
        decision,
        results,
        limitations: vec![
            "Absolute timing and resident-set values are runner-specific; compare phase shape and thresholds, not isolated cross-host deltas.",
            "The matrix contains two public workspaces and does not represent every language, filesystem, repository shape, or antivirus configuration.",
            "Linux and macOS use operating-system time high-water counters; Windows uses a 20 ms working-set sampler and may miss a shorter peak.",
            "Watcher delivery includes one configured quiet period and the native notify backend but excludes reconciliation work.",
            "Overflow and process interruption remain deterministic correctness gates rather than portable on-demand performance events.",
        ],
    })
}

fn validate_profile(
    manifest: &Manifest,
    corpus: &Corpus,
    profile: &Profile,
    runtime_revision: &mut Option<String>,
) -> AnyResult<()> {
    if profile.schema_version != 7
        || !profile.release_build
        || profile.leantoken_worktree_dirty != Some(false)
    {
        return Err(invalid_data(
            "profile must be clean schema-v7 release evidence",
        ));
    }
    if profile.corpus.revision.as_deref() != Some(corpus.revision.as_str())
        || profile.corpus.source_repository.as_deref() != Some(corpus.repository.as_str())
    {
        return Err(invalid_data(
            "profile corpus identity does not match manifest",
        ));
    }
    if profile.corpus.indexing_iterations != manifest.profile.iterations
        || profile.corpus.read_samples != manifest.profile.read_samples
        || profile.corpus.hot_set_files != manifest.profile.hot_set
        || profile.full_noop.timing.samples != manifest.profile.iterations
        || profile.targeted_changed.timing.samples != manifest.profile.iterations
        || profile.watcher_modify_delivery.timing.samples != manifest.profile.iterations
        || profile.watcher_modify_delivery.configured_debounce_ms
            != manifest.profile.watcher_debounce_ms
    {
        return Err(invalid_data(
            "profile sample policy does not match manifest",
        ));
    }
    if profile.watcher_modify_delivery.changed_messages
        + profile.watcher_modify_delivery.full_reconciliation_messages
        != manifest.profile.iterations
    {
        return Err(invalid_data(
            "watcher delivery did not account for every sample",
        ));
    }
    let revision = profile
        .leantoken_git_revision
        .as_ref()
        .ok_or_else(|| invalid_data("profile omitted runtime revision"))?;
    match runtime_revision {
        Some(expected) if expected != revision => {
            return Err(invalid_data("matrix mixed LeanToken revisions"));
        }
        None => *runtime_revision = Some(revision.clone()),
        Some(_) => {}
    }
    Ok(())
}

fn pair_result(
    manifest: &Manifest,
    corpus: &Corpus,
    platform: &str,
    profile: Profile,
    peak: PeakRss,
) -> AnyResult<PairResult> {
    if peak.peak_rss_bytes == 0 {
        return Err(invalid_data("peak RSS must be positive"));
    }
    let directory = profile
        .directory_rename_delta
        .ok_or_else(|| invalid_data("monorepo profile omitted directory rename"))?;
    let scan_share = percent(
        profile.full_noop_phases.discovery.p95_us + profile.full_noop_phases.hash_and_plan.p95_us,
        profile.full_noop_phases.total.p95_us,
    );
    let full_noop_p95_ms = milliseconds(profile.full_noop.timing.p95_us);
    let material_full_scan = full_noop_p95_ms >= manifest.adoption_threshold.full_fallback_p95_ms
        && scan_share
            >= manifest
                .adoption_threshold
                .discovery_and_hash_plan_share_percent;
    Ok(PairResult {
        platform: platform.to_string(),
        host_os: profile.host_os,
        host_arch: profile.host_arch,
        corpus_id: corpus.id.clone(),
        corpus_repository: corpus.repository.clone(),
        corpus_revision: corpus.revision.clone(),
        files: profile.corpus.files,
        total_bytes: profile.corpus.total_bytes,
        max_directory_depth: profile.corpus.max_directory_depth,
        peak_rss_bytes: peak.peak_rss_bytes,
        peak_rss_source: peak.source,
        initial_index_ms: profile.initial_index.elapsed_ms,
        initial_storage_footprint: profile.initial_index.storage_footprint,
        final_storage_footprint: profile.final_storage_footprint,
        full_noop_p50_ms: milliseconds(profile.full_noop.timing.p50_us),
        full_noop_p95_ms,
        discovery_p50_ms: milliseconds(profile.full_noop_phases.discovery.p50_us),
        discovery_p95_ms: milliseconds(profile.full_noop_phases.discovery.p95_us),
        hash_and_plan_p50_ms: milliseconds(profile.full_noop_phases.hash_and_plan.p50_us),
        hash_and_plan_p95_ms: milliseconds(profile.full_noop_phases.hash_and_plan.p95_us),
        preparation_p95_ms: milliseconds(profile.full_noop_phases.preparation.p95_us),
        insertion_p95_ms: milliseconds(profile.full_noop_phases.insertion.p95_us),
        publication_p95_ms: milliseconds(profile.full_noop_phases.publication.p95_us),
        dominant_full_noop_phase: dominant_phase(&profile.full_noop_phases),
        discovery_and_hash_plan_share_percent: scan_share,
        targeted_modify_p50_ms: milliseconds(profile.targeted_changed.timing.p50_us),
        targeted_modify_p95_ms: milliseconds(profile.targeted_changed.timing.p95_us),
        create_p50_ms: milliseconds(profile.create_delta.timing.p50_us),
        create_p95_ms: milliseconds(profile.create_delta.timing.p95_us),
        delete_p50_ms: milliseconds(profile.delete_targeted.timing.p50_us),
        delete_p95_ms: milliseconds(profile.delete_targeted.timing.p95_us),
        file_rename_p50_ms: milliseconds(profile.rename_delta.timing.p50_us),
        file_rename_p95_ms: milliseconds(profile.rename_delta.timing.p95_us),
        directory_rename_affected_files: directory.affected_files,
        directory_rename_p50_ms: milliseconds(directory.indexing.timing.p50_us),
        directory_rename_p95_ms: milliseconds(directory.indexing.timing.p95_us),
        semantic_ignore_p50_ms: milliseconds(profile.ignore_visibility_delta.timing.p50_us),
        semantic_ignore_p95_ms: milliseconds(profile.ignore_visibility_delta.timing.p95_us),
        watcher_delivery_p50_ms: milliseconds(profile.watcher_modify_delivery.timing.p50_us),
        watcher_delivery_p95_ms: milliseconds(profile.watcher_modify_delivery.timing.p95_us),
        watcher_changed_messages: profile.watcher_modify_delivery.changed_messages,
        watcher_full_reconciliation_messages: profile
            .watcher_modify_delivery
            .full_reconciliation_messages,
        material_full_scan,
    })
}

fn dominant_phase(phases: &PhaseMeasurement) -> &'static str {
    [
        ("discovery", phases.discovery.p95_us),
        ("hash_and_plan", phases.hash_and_plan.p95_us),
        ("preparation", phases.preparation.p95_us),
        ("insertion", phases.insertion.p95_us),
    ]
    .into_iter()
    .max_by(|left, right| left.1.total_cmp(&right.1))
    .map_or("unknown", |phase| phase.0)
}

fn milliseconds(microseconds: f64) -> f64 {
    microseconds / 1_000.0
}

fn percent(numerator: f64, denominator: f64) -> f64 {
    if denominator > 0.0 {
        numerator / denominator * 100.0
    } else {
        0.0
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> AnyResult<T> {
    serde_json::from_slice(&fs::read(path)?).map_err(Into::into)
}

fn invalid_data(message: &str) -> Box<dyn Error> {
    Box::new(io::Error::new(io::ErrorKind::InvalidData, message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn materiality_requires_both_absolute_cost_and_scan_share() {
        let threshold = AdoptionThreshold {
            full_fallback_p95_ms: 250.0,
            discovery_and_hash_plan_share_percent: 50.0,
            minimum_material_corpus_platform_pairs: 2,
            rule: String::new(),
        };
        let material = |latency: f64, share: f64| {
            latency >= threshold.full_fallback_p95_ms
                && share >= threshold.discovery_and_hash_plan_share_percent
        };

        assert!(material(250.0, 50.0));
        assert!(!material(249.9, 100.0));
        assert!(!material(1_000.0, 49.9));
    }

    #[test]
    fn dominant_phase_uses_p95_work_excluding_overlapping_publication() {
        let timing = |p95_us| TimingStats {
            samples: 10,
            p50_us: p95_us,
            p95_us,
        };
        let phases = PhaseMeasurement {
            total: timing(100.0),
            discovery: timing(20.0),
            hash_and_plan: timing(60.0),
            preparation: timing(0.0),
            insertion: timing(0.0),
            publication: timing(100.0),
        };

        assert_eq!(dominant_phase(&phases), "hash_and_plan");
    }
}
