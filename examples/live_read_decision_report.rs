use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use clap::Parser;
use serde::{Deserialize, Serialize};

type AnyResult<T> = Result<T, Box<dyn Error>>;

const PLATFORMS: &[&str] = &["ubuntu-latest", "macos-latest", "windows-latest"];

#[derive(Debug, Parser)]
#[command(about = "Aggregate the frozen cross-platform live-read decision matrix")]
struct Args {
    /// Frozen experiment, corpus, and threshold manifest.
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
    hot_set: usize,
    max_tokens: usize,
    pressure_bytes: usize,
    candidate_cache_bytes: u64,
}

#[derive(Debug, Deserialize, Serialize)]
struct AdoptionThreshold {
    minimum_avoidable_read_p95_ms: f64,
    minimum_live_read_share_percent: f64,
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
    initial_index_ms: f64,
    post_index_spread_direct: ReadMeasurement,
    warm_hot_direct: ReadMeasurement,
    warm_hot_memory_copy: ReadMeasurement,
    warm_hot_service: ServiceMeasurement,
    warm_spread_service: ServiceMeasurement,
    pressure: PressureMeasurement,
    live_change: LiveChangeCheck,
    hot_payload_bytes: u64,
}

#[derive(Debug, Deserialize)]
struct ProfileCorpus {
    source_repository: String,
    revision: String,
    files: usize,
    total_bytes: u64,
    text_read_targets: usize,
    iterations: usize,
    hot_set_files: usize,
    max_tokens: usize,
    pressure_bytes: usize,
}

#[derive(Debug, Deserialize)]
struct ReadMeasurement {
    timing: TimingStats,
    mean_bytes: f64,
    unique_files: usize,
}

#[derive(Debug, Deserialize)]
struct ServiceMeasurement {
    request: TimingStats,
    serialization: TimingStats,
    complete: TimingStats,
    mean_source_bytes: f64,
    mean_wire_bytes: f64,
    unique_files: usize,
}

#[derive(Debug, Deserialize)]
struct PressureMeasurement {
    retained_touched_bytes: usize,
    direct_hot: ReadMeasurement,
    service_hot: ServiceMeasurement,
}

#[derive(Debug, Deserialize)]
struct LiveChangeCheck {
    generation_before: u64,
    stale_generation: u64,
    generation_after: u64,
    stale_before_reconciliation: bool,
    current_after_reconciliation: bool,
}

#[derive(Debug, Deserialize)]
struct TimingStats {
    samples: usize,
    mean_us: f64,
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
    prototype_hot_file_cache: bool,
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
    text_read_targets: usize,
    initial_index_ms: f64,
    peak_rss_bytes: u64,
    peak_rss_source: String,
    post_index_spread_direct_p50_ms: f64,
    post_index_spread_direct_p95_ms: f64,
    warm_hot_direct_p50_ms: f64,
    warm_hot_direct_p95_ms: f64,
    warm_hot_memory_copy_p95_ms: f64,
    warm_hot_service_request_p50_ms: f64,
    warm_hot_service_request_p95_ms: f64,
    warm_hot_serialization_p95_ms: f64,
    warm_hot_complete_p50_ms: f64,
    warm_hot_complete_p95_ms: f64,
    warm_spread_complete_p95_ms: f64,
    pressure_direct_p50_ms: f64,
    pressure_direct_p95_ms: f64,
    pressure_complete_p50_ms: f64,
    pressure_complete_p95_ms: f64,
    warm_live_read_mean_share_percent: f64,
    pressure_live_read_mean_share_percent: f64,
    decision_avoidable_read_p95_ms: f64,
    decision_live_read_share_percent: f64,
    hot_payload_bytes: u64,
    candidate_cache_bytes: u64,
    live_change_generation_before: u64,
    live_change_generation_after: u64,
    material_live_read_cost: bool,
}

fn main() -> AnyResult<()> {
    let args = Args::parse();
    let manifest: Manifest = read_json(&args.manifest)?;
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
    validate_manifest(manifest)?;
    let mut results = Vec::with_capacity(manifest.corpora.len() * PLATFORMS.len());
    let mut runtime_revision = None;
    for corpus in &manifest.corpora {
        for platform in PLATFORMS {
            let directory = artifacts.join(format!("live-read-profile-{platform}-{}", corpus.id));
            let profile: Profile = read_json(&directory.join("profile.json"))?;
            let peak: PeakRss = read_json(&directory.join("peak-rss.json"))?;
            validate_profile(manifest, corpus, &profile, &mut runtime_revision)?;
            results.push(pair_result(manifest, corpus, platform, profile, peak)?);
        }
    }
    let material_pairs = results
        .iter()
        .filter(|result| result.material_live_read_cost)
        .count();
    let prototype_hot_file_cache = material_pairs
        >= manifest
            .adoption_threshold
            .minimum_material_corpus_platform_pairs;
    let decision = if prototype_hot_file_cache {
        format!(
            "prototype eligible: {material_pairs} corpus/platform pairs met the frozen live-read threshold"
        )
    } else {
        format!(
            "no-cache: {material_pairs} corpus/platform pairs met the frozen live-read threshold; retain bounded live filesystem reads and the operating-system page cache"
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
            hot_set: manifest.profile.hot_set,
            max_tokens: manifest.profile.max_tokens,
            pressure_bytes: manifest.profile.pressure_bytes,
            candidate_cache_bytes: manifest.profile.candidate_cache_bytes,
        },
        adoption_threshold: AdoptionThreshold {
            minimum_avoidable_read_p95_ms: manifest
                .adoption_threshold
                .minimum_avoidable_read_p95_ms,
            minimum_live_read_share_percent: manifest
                .adoption_threshold
                .minimum_live_read_share_percent,
            minimum_material_corpus_platform_pairs: manifest
                .adoption_threshold
                .minimum_material_corpus_platform_pairs,
            rule: manifest.adoption_threshold.rule.clone(),
        },
        expected_pairs: results.len(),
        material_pairs,
        prototype_hot_file_cache,
        decision,
        results,
        limitations: vec![
            "The checkout copy and initial index touch corpus files; the first profile pass is not proof of a cold operating-system page cache.",
            "The pressure condition retains and touches process memory but cannot force or verify cross-platform page-cache eviction.",
            "Direct-read p95 is a generous avoidable-cost upper bound that excludes real cache lookup, synchronization, cloning, eviction, and invalidation work.",
            "No provider or model runs in this profile, so the direct-read share of service-plus-serialization time is an upper bound for an agent request.",
            "GitHub-hosted local filesystems do not represent remote, encrypted, antivirus-heavy, or contended deployments.",
            "Linux and macOS use operating-system RSS high-water counters; Windows uses a 20 ms working-set sampler and may miss a shorter peak.",
        ],
    })
}

fn validate_manifest(manifest: &Manifest) -> AnyResult<()> {
    if manifest.schema_version != 1
        || manifest.corpora.is_empty()
        || manifest.profile.iterations == 0
        || manifest.profile.hot_set == 0
        || manifest.profile.max_tokens == 0
        || manifest.profile.candidate_cache_bytes == 0
        || !manifest
            .adoption_threshold
            .minimum_avoidable_read_p95_ms
            .is_finite()
        || manifest.adoption_threshold.minimum_avoidable_read_p95_ms <= 0.0
        || !manifest
            .adoption_threshold
            .minimum_live_read_share_percent
            .is_finite()
        || manifest.adoption_threshold.minimum_live_read_share_percent <= 0.0
        || manifest
            .adoption_threshold
            .minimum_material_corpus_platform_pairs
            == 0
    {
        return Err(invalid_data("invalid live-read decision manifest"));
    }
    Ok(())
}

fn validate_profile(
    manifest: &Manifest,
    corpus: &Corpus,
    profile: &Profile,
    runtime_revision: &mut Option<String>,
) -> AnyResult<()> {
    if profile.schema_version != 1
        || !profile.release_build
        || profile.leantoken_worktree_dirty != Some(false)
    {
        return Err(invalid_data(
            "profile must be clean schema-v1 release evidence",
        ));
    }
    if profile.corpus.revision != corpus.revision
        || profile.corpus.source_repository != corpus.repository
    {
        return Err(invalid_data(
            "profile corpus identity does not match manifest",
        ));
    }
    let expected_hot_set = manifest
        .profile
        .hot_set
        .min(profile.corpus.text_read_targets);
    if profile.corpus.iterations != manifest.profile.iterations
        || profile.corpus.hot_set_files != expected_hot_set
        || profile.corpus.max_tokens != manifest.profile.max_tokens
        || profile.corpus.pressure_bytes != manifest.profile.pressure_bytes
        || profile.pressure.retained_touched_bytes != manifest.profile.pressure_bytes
        || profile.post_index_spread_direct.timing.samples != manifest.profile.iterations
        || profile.warm_hot_direct.timing.samples != manifest.profile.iterations
        || profile.warm_hot_memory_copy.timing.samples != manifest.profile.iterations
        || profile.warm_hot_service.complete.samples != manifest.profile.iterations
        || profile.warm_spread_service.complete.samples != manifest.profile.iterations
        || profile.pressure.direct_hot.timing.samples != manifest.profile.iterations
        || profile.pressure.service_hot.complete.samples != manifest.profile.iterations
        || profile.warm_hot_direct.unique_files != expected_hot_set
        || profile.warm_hot_service.unique_files != expected_hot_set
        || profile.pressure.direct_hot.unique_files != expected_hot_set
        || profile.pressure.service_hot.unique_files != expected_hot_set
    {
        return Err(invalid_data(
            "profile sample policy does not match manifest",
        ));
    }
    if profile.corpus.files == 0
        || profile.corpus.text_read_targets == 0
        || profile.corpus.total_bytes == 0
        || profile.live_change.stale_generation != profile.live_change.generation_before
        || profile.live_change.generation_after <= profile.live_change.generation_before
        || !profile.live_change.stale_before_reconciliation
        || !profile.live_change.current_after_reconciliation
    {
        return Err(invalid_data(
            "profile corpus or live-change correctness evidence is invalid",
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
    validate_timing(&profile.post_index_spread_direct.timing)?;
    validate_timing(&profile.warm_hot_direct.timing)?;
    validate_timing(&profile.warm_hot_memory_copy.timing)?;
    validate_service(&profile.warm_hot_service)?;
    validate_service(&profile.warm_spread_service)?;
    validate_timing(&profile.pressure.direct_hot.timing)?;
    validate_service(&profile.pressure.service_hot)?;
    if peak.peak_rss_bytes == 0
        || profile.hot_payload_bytes == 0
        || profile.warm_hot_direct.mean_bytes <= 0.0
        || profile.warm_hot_memory_copy.mean_bytes != profile.warm_hot_direct.mean_bytes
        || profile.warm_hot_service.mean_source_bytes != profile.warm_hot_direct.mean_bytes
        || profile.pressure.direct_hot.mean_bytes != profile.warm_hot_direct.mean_bytes
        || profile.pressure.service_hot.mean_source_bytes != profile.warm_hot_direct.mean_bytes
    {
        return Err(invalid_data("profile byte or RSS accounting is invalid"));
    }
    let warm_share = percent_share(
        profile.warm_hot_direct.timing.mean_us,
        profile.warm_hot_service.complete.mean_us,
    )?;
    let pressure_share = percent_share(
        profile.pressure.direct_hot.timing.mean_us,
        profile.pressure.service_hot.complete.mean_us,
    )?;
    let avoidable_p95_ms = profile
        .warm_hot_direct
        .timing
        .p95_us
        .max(profile.pressure.direct_hot.timing.p95_us)
        / 1_000.0;
    let decision_share = warm_share.max(pressure_share);
    let material = classify_material(
        avoidable_p95_ms,
        decision_share,
        &manifest.adoption_threshold,
    );
    Ok(PairResult {
        platform: platform.to_string(),
        host_os: profile.host_os,
        host_arch: profile.host_arch,
        corpus_id: corpus.id.clone(),
        corpus_repository: corpus.repository.clone(),
        corpus_revision: corpus.revision.clone(),
        files: profile.corpus.files,
        total_bytes: profile.corpus.total_bytes,
        text_read_targets: profile.corpus.text_read_targets,
        initial_index_ms: profile.initial_index_ms,
        peak_rss_bytes: peak.peak_rss_bytes,
        peak_rss_source: peak.source,
        post_index_spread_direct_p50_ms: ms(profile.post_index_spread_direct.timing.p50_us),
        post_index_spread_direct_p95_ms: ms(profile.post_index_spread_direct.timing.p95_us),
        warm_hot_direct_p50_ms: ms(profile.warm_hot_direct.timing.p50_us),
        warm_hot_direct_p95_ms: ms(profile.warm_hot_direct.timing.p95_us),
        warm_hot_memory_copy_p95_ms: ms(profile.warm_hot_memory_copy.timing.p95_us),
        warm_hot_service_request_p50_ms: ms(profile.warm_hot_service.request.p50_us),
        warm_hot_service_request_p95_ms: ms(profile.warm_hot_service.request.p95_us),
        warm_hot_serialization_p95_ms: ms(profile.warm_hot_service.serialization.p95_us),
        warm_hot_complete_p50_ms: ms(profile.warm_hot_service.complete.p50_us),
        warm_hot_complete_p95_ms: ms(profile.warm_hot_service.complete.p95_us),
        warm_spread_complete_p95_ms: ms(profile.warm_spread_service.complete.p95_us),
        pressure_direct_p50_ms: ms(profile.pressure.direct_hot.timing.p50_us),
        pressure_direct_p95_ms: ms(profile.pressure.direct_hot.timing.p95_us),
        pressure_complete_p50_ms: ms(profile.pressure.service_hot.complete.p50_us),
        pressure_complete_p95_ms: ms(profile.pressure.service_hot.complete.p95_us),
        warm_live_read_mean_share_percent: warm_share,
        pressure_live_read_mean_share_percent: pressure_share,
        decision_avoidable_read_p95_ms: avoidable_p95_ms,
        decision_live_read_share_percent: decision_share,
        hot_payload_bytes: profile.hot_payload_bytes,
        candidate_cache_bytes: manifest.profile.candidate_cache_bytes,
        live_change_generation_before: profile.live_change.generation_before,
        live_change_generation_after: profile.live_change.generation_after,
        material_live_read_cost: material,
    })
}

fn validate_service(measurement: &ServiceMeasurement) -> AnyResult<()> {
    validate_timing(&measurement.request)?;
    validate_timing(&measurement.serialization)?;
    validate_timing(&measurement.complete)?;
    if measurement.request.samples != measurement.complete.samples
        || measurement.serialization.samples != measurement.complete.samples
        || !measurement.mean_source_bytes.is_finite()
        || measurement.mean_source_bytes <= 0.0
        || !measurement.mean_wire_bytes.is_finite()
        || measurement.mean_wire_bytes <= 0.0
    {
        return Err(invalid_data("service timing or byte accounting is invalid"));
    }
    Ok(())
}

fn validate_timing(timing: &TimingStats) -> AnyResult<()> {
    if timing.samples == 0
        || !timing.mean_us.is_finite()
        || timing.mean_us <= 0.0
        || !timing.p50_us.is_finite()
        || timing.p50_us <= 0.0
        || !timing.p95_us.is_finite()
        || timing.p95_us < timing.p50_us
    {
        return Err(invalid_data("timing distribution is invalid"));
    }
    Ok(())
}

fn percent_share(part: f64, total: f64) -> AnyResult<f64> {
    if !part.is_finite() || !total.is_finite() || part <= 0.0 || total <= 0.0 {
        return Err(invalid_data("cannot compute live-read share"));
    }
    Ok(part / total * 100.0)
}

fn classify_material(
    avoidable_p95_ms: f64,
    live_read_share_percent: f64,
    threshold: &AdoptionThreshold,
) -> bool {
    avoidable_p95_ms >= threshold.minimum_avoidable_read_p95_ms
        && live_read_share_percent >= threshold.minimum_live_read_share_percent
}

fn ms(microseconds: f64) -> f64 {
    microseconds / 1_000.0
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

    fn threshold() -> AdoptionThreshold {
        AdoptionThreshold {
            minimum_avoidable_read_p95_ms: 1.0,
            minimum_live_read_share_percent: 10.0,
            minimum_material_corpus_platform_pairs: 2,
            rule: "fixture".into(),
        }
    }

    #[test]
    fn materiality_requires_absolute_cost_and_request_share() {
        assert!(classify_material(1.0, 10.0, &threshold()));
        assert!(!classify_material(0.999, 50.0, &threshold()));
        assert!(!classify_material(10.0, 9.999, &threshold()));
    }

    #[test]
    fn share_uses_means_from_the_same_measurement_condition() {
        assert_eq!(percent_share(20.0, 200.0).expect("share"), 10.0);
        assert!(percent_share(0.0, 200.0).is_err());
        assert!(percent_share(20.0, f64::NAN).is_err());
    }
}
