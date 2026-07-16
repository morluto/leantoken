use std::collections::BTreeMap;
use std::error::Error;
use std::fs::{self, OpenOptions};
use std::hint::black_box;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;
use leantoken::Config;
use leantoken::indexer::Indexer;
use leantoken::model::IndexResponse;
use leantoken::repository::{DiscoveredFile, discover_files};
use leantoken::storage::Storage;
use serde::Serialize;

type AnyResult<T> = Result<T, Box<dyn Error>>;

#[derive(Debug, Parser)]
#[command(about = "Profile full and changed-path indexing plus warm file reads")]
struct Args {
    /// Existing clean Git checkout to profile through a disposable snapshot.
    #[arg(long, value_name = "PATH")]
    repository: Option<PathBuf>,
    /// Public corpus name or URL to include in the report.
    #[arg(long, requires = "repository")]
    repository_label: Option<String>,
    /// Number of synthetic Rust source files.
    #[arg(long, default_value_t = 2_000)]
    files: usize,
    /// Approximate bytes in each synthetic source file.
    #[arg(long, default_value_t = 8 * 1024)]
    file_bytes: usize,
    /// Samples for each indexing measurement.
    #[arg(long, default_value_t = 10)]
    iterations: usize,
    /// Samples for each file-read measurement.
    #[arg(long, default_value_t = 2_000)]
    read_samples: usize,
    /// Number of files in the repeated-read working set.
    #[arg(long, default_value_t = 8)]
    hot_set: usize,
    /// JSON report destination.
    #[arg(long, default_value = "target/indexing_profile_report.json")]
    output: PathBuf,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    leantoken_version: &'static str,
    leantoken_git_revision: Option<String>,
    leantoken_worktree_dirty: Option<bool>,
    host_os: &'static str,
    host_arch: &'static str,
    release_build: bool,
    corpus: CorpusReport,
    initial_index: IndexSample,
    full_noop: IndexMeasurement,
    full_changed: IndexMeasurement,
    targeted_changed: IndexMeasurement,
    create_delta: IndexMeasurement,
    delete_targeted: IndexMeasurement,
    rename_delta: IndexMeasurement,
    ignore_change_delta: IndexMeasurement,
    warm_hot_file_reads: ReadMeasurement,
    warm_spread_file_reads: ReadMeasurement,
    memory_hot_file_copies: ReadMeasurement,
    limitations: Vec<String>,
}

#[derive(Debug, Serialize)]
struct CorpusReport {
    source_kind: &'static str,
    source_repository: Option<String>,
    revision: Option<String>,
    files: usize,
    total_bytes: u64,
    mean_file_bytes: f64,
    max_directory_depth: usize,
    extensions: BTreeMap<String, usize>,
    synthetic_file_bytes: Option<usize>,
    indexing_iterations: usize,
    read_samples: usize,
    hot_set_files: usize,
}

#[derive(Debug, Serialize)]
struct IndexSample {
    elapsed_ms: f64,
    response: IndexResponse,
}

#[derive(Debug, Serialize)]
struct IndexMeasurement {
    timing: TimingStats,
    files_seen_per_sample: usize,
    files_indexed_per_sample: usize,
    files_removed_per_sample: usize,
}

#[derive(Debug, Clone, Copy)]
struct ExpectedIndexCounts {
    seen: usize,
    indexed: usize,
    removed: usize,
}

#[derive(Debug, Serialize)]
struct ReadMeasurement {
    timing: TimingStats,
    total_bytes: u64,
    mean_bytes: f64,
}

#[derive(Debug, Serialize)]
struct TimingStats {
    samples: usize,
    total_ms: f64,
    mean_us: f64,
    min_us: f64,
    p50_us: f64,
    p95_us: f64,
    max_us: f64,
}

fn main() -> AnyResult<()> {
    let args = Args::parse();
    validate_args(&args)?;
    let report = run_profile(&args)?;
    let json = serde_json::to_string_pretty(&report)?;
    if let Some(parent) = args.output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&args.output, format!("{json}\n"))?;
    println!("{json}");
    Ok(())
}

fn validate_args(args: &Args) -> AnyResult<()> {
    if args.repository.is_none() && args.files < 2 {
        return Err(invalid_input("--files must be at least 2"));
    }
    if args.repository.is_none() && args.file_bytes < 128 {
        return Err(invalid_input("--file-bytes must be at least 128"));
    }
    if args
        .repository
        .as_ref()
        .is_some_and(|repository| !repository.is_dir())
    {
        return Err(invalid_input("--repository must name a Git checkout"));
    }
    if args.iterations == 0 {
        return Err(invalid_input("--iterations must be positive"));
    }
    if args.read_samples == 0 {
        return Err(invalid_input("--read-samples must be positive"));
    }
    if args.hot_set == 0 {
        return Err(invalid_input("--hot-set must be positive"));
    }
    Ok(())
}

fn invalid_input(message: &str) -> Box<dyn Error> {
    Box::new(io::Error::new(io::ErrorKind::InvalidInput, message))
}

fn run_profile(args: &Args) -> AnyResult<Report> {
    let corpus = prepare_corpus(args)?;
    let ignore_path = corpus.root.join(".gitignore");
    if ignore_path.exists() {
        if !fs::symlink_metadata(&ignore_path)?.file_type().is_file() {
            return Err(invalid_input("profile .gitignore must be a regular file"));
        }
    } else {
        fs::write(&ignore_path, "# LeanToken indexing profile\n")?;
    }
    let database = tempfile::tempdir()?;
    let config = Arc::new(Config::discover(
        &corpus.root,
        Some(database.path().join("index.sqlite")),
    )?);
    let discovered = discover_files(&corpus.root, config.max_file_bytes)?;
    if discovered.len() < 2 {
        return Err(invalid_input(
            "profile corpus must contain at least two ignore-visible files within max_file_bytes",
        ));
    }
    let paths = discovered
        .iter()
        .map(|file| file.absolute_path.clone())
        .collect::<Vec<_>>();
    let mutation_files = discovered
        .iter()
        .filter(|file| {
            Path::new(&file.relative_path)
                .file_name()
                .is_some_and(|name| name != ".gitignore" && name != ".ignore")
                && fs::read_to_string(&file.absolute_path).is_ok()
        })
        .take(2)
        .collect::<Vec<_>>();
    if mutation_files.len() < 2 {
        return Err(invalid_input(
            "profile corpus must contain at least two UTF-8 files for mutation measurements",
        ));
    }
    let storage = Storage::open(&config.database_path)?;
    let indexer = Indexer::new(config, storage)?;

    let start = Instant::now();
    let initial_response = indexer.reconcile(false)?;
    let initial_index = IndexSample {
        elapsed_ms: milliseconds(start.elapsed()),
        response: initial_response,
    };

    let mut full_noop_durations = Vec::with_capacity(args.iterations);
    for _ in 0..args.iterations {
        let start = Instant::now();
        let response = indexer.reconcile(false)?;
        full_noop_durations.push(start.elapsed());
        require_index_counts(&response, discovered.len(), 0, "full no-op")?;
    }

    let full_changed = measure_changed_indexing(
        args.iterations,
        &mutation_files[0].absolute_path,
        || indexer.reconcile(false),
        discovered.len(),
        "full changed",
    )?;
    let targeted_changed = measure_changed_indexing(
        args.iterations,
        &mutation_files[1].absolute_path,
        || indexer.reconcile_paths(&[mutation_files[1].relative_path.clone()]),
        1,
        "targeted changed",
    )?;
    let create_path = corpus.root.join("leantoken_profile_created.rs");
    let create_relative = "leantoken_profile_created.rs".to_string();
    if create_path.exists() {
        return Err(invalid_input(
            "profile corpus already contains leantoken_profile_created.rs",
        ));
    }
    let create_delta = measure_lifecycle_indexing(
        args.iterations,
        |iteration| fs::write(&create_path, synthetic_source(iteration, 256)),
        || indexer.reconcile_paths(std::slice::from_ref(&create_relative)),
        || {
            fs::remove_file(&create_path)?;
            indexer.reconcile_paths(std::slice::from_ref(&create_relative))?;
            Ok(())
        },
        ExpectedIndexCounts {
            seen: 1,
            indexed: 1,
            removed: 0,
        },
        "create delta",
    )?;

    let delete_path = mutation_files[0].absolute_path.clone();
    let delete_relative = mutation_files[0].relative_path.clone();
    let delete_content = fs::read(&delete_path)?;
    let delete_targeted = measure_lifecycle_indexing(
        args.iterations,
        |_| fs::remove_file(&delete_path),
        || indexer.reconcile_paths(std::slice::from_ref(&delete_relative)),
        || {
            fs::write(&delete_path, &delete_content)?;
            indexer.reconcile(false)?;
            Ok(())
        },
        ExpectedIndexCounts {
            seen: 1,
            indexed: 0,
            removed: 1,
        },
        "targeted delete",
    )?;

    let rename_path = mutation_files[1].absolute_path.clone();
    let rename_relative = mutation_files[1].relative_path.clone();
    let rename_destination = rename_path.with_file_name("leantoken_profile_renamed.rs");
    if rename_destination.exists() {
        return Err(invalid_input(
            "profile corpus already contains leantoken_profile_renamed.rs",
        ));
    }
    let rename_destination_relative = Path::new(&rename_relative)
        .with_file_name("leantoken_profile_renamed.rs")
        .to_string_lossy()
        .replace('\\', "/");
    let rename_paths = vec![rename_relative.clone(), rename_destination_relative.clone()];
    let rename_delta = measure_lifecycle_indexing(
        args.iterations,
        |_| fs::rename(&rename_path, &rename_destination),
        || indexer.reconcile_paths(&rename_paths),
        || {
            fs::rename(&rename_destination, &rename_path)?;
            indexer.reconcile(false)?;
            Ok(())
        },
        ExpectedIndexCounts {
            seen: 2,
            indexed: 1,
            removed: 1,
        },
        "rename delta",
    )?;

    let ignore_relative = ".gitignore".to_string();
    let ignore_change_delta = measure_lifecycle_indexing(
        args.iterations,
        |iteration| {
            let mut file = OpenOptions::new().append(true).open(&ignore_path)?;
            writeln!(file, "# profile ignore mutation {iteration}")
        },
        || indexer.reconcile_paths(std::slice::from_ref(&ignore_relative)),
        || Ok(()),
        ExpectedIndexCounts {
            seen: 1,
            indexed: 1,
            removed: 0,
        },
        "ignore-change delta",
    )?;

    let hot_set = args.hot_set.min(paths.len());
    let hot_paths = &paths[..hot_set];
    let hot_contents = hot_paths
        .iter()
        .map(fs::read)
        .collect::<Result<Vec<_>, _>>()?;
    for path in hot_paths {
        black_box(fs::read(path)?);
    }

    let warm_hot_file_reads = measure_file_reads(hot_paths, args.read_samples)?;
    let warm_spread_file_reads = measure_file_reads(&paths, args.read_samples)?;
    let memory_hot_file_copies = measure_memory_copies(&hot_contents, args.read_samples);

    let (leantoken_git_revision, leantoken_worktree_dirty) = leantoken_source_identity();
    Ok(Report {
        schema_version: 3,
        leantoken_version: env!("CARGO_PKG_VERSION"),
        leantoken_git_revision,
        leantoken_worktree_dirty,
        host_os: std::env::consts::OS,
        host_arch: std::env::consts::ARCH,
        release_build: !cfg!(debug_assertions),
        corpus: corpus_report(args, &corpus, &discovered, hot_set),
        initial_index,
        full_noop: IndexMeasurement {
            timing: TimingStats::from_durations(full_noop_durations),
            files_seen_per_sample: discovered.len(),
            files_indexed_per_sample: 0,
            files_removed_per_sample: 0,
        },
        full_changed,
        targeted_changed,
        create_delta,
        delete_targeted,
        rename_delta,
        ignore_change_delta,
        warm_hot_file_reads,
        warm_spread_file_reads,
        memory_hot_file_copies,
        limitations: vec![
            corpus.limitation.to_string(),
            "File-read measurements use the operating system's warm page cache; they do not represent cold, remote, encrypted, or heavily contended filesystems.".into(),
            "The in-memory comparison copies bytes but excludes cache lookup, eviction, synchronization, invalidation, and memory-pressure costs.".into(),
            "Lifecycle measurements invoke the paths emitted by the watcher directly; they do not include notify backend or debounce latency.".into(),
            "Watcher-overflow and interrupted reconciliation still require separate stress measurements.".into(),
            "Timing is machine-specific. Compare runs only on the same host and build profile, and use release builds for decisions.".into(),
        ],
    })
}

struct PreparedCorpus {
    _temporary_root: tempfile::TempDir,
    root: PathBuf,
    source_kind: &'static str,
    source_repository: Option<String>,
    revision: Option<String>,
    limitation: &'static str,
}

fn prepare_corpus(args: &Args) -> AnyResult<PreparedCorpus> {
    let temporary_root = tempfile::tempdir()?;
    if let Some(repository) = &args.repository {
        let repository = repository.canonicalize()?;
        let revision = git_output(&repository, ["rev-parse", "HEAD"])?;
        let status = git_output(
            &repository,
            ["status", "--porcelain", "--untracked-files=all"],
        )?;
        if !status.is_empty() {
            return Err(invalid_input(
                "--repository must be clean so the recorded revision identifies the corpus",
            ));
        }
        let root = temporary_root.path().join("repository");
        snapshot_repository(&repository, &root, temporary_root.path())?;
        return Ok(PreparedCorpus {
            _temporary_root: temporary_root,
            root,
            source_kind: "git_worktree_snapshot",
            source_repository: args.repository_label.clone(),
            revision: Some(revision),
            limitation: "The repository profile uses an isolated ignore-aware snapshot of a clean checkout at the recorded commit.",
        });
    }

    create_corpus(temporary_root.path(), args.files, args.file_bytes)?;
    Ok(PreparedCorpus {
        root: temporary_root.path().to_path_buf(),
        _temporary_root: temporary_root,
        source_kind: "synthetic_rust",
        source_repository: None,
        revision: None,
        limitation: "The generated Rust corpus controls file count and size but does not model a real repository's language mix or directory shape.",
    })
}

fn snapshot_repository(source: &Path, destination: &Path, temporary_root: &Path) -> AnyResult<()> {
    let source_config = Config::discover(source, Some(temporary_root.join("source-probe.sqlite")))?;
    let files = discover_files(source, source_config.max_file_bytes)?;
    fs::create_dir_all(destination)?;
    for file in files {
        let target = destination.join(Path::new(&file.relative_path));
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&file.absolute_path, target)?;
    }
    Ok(())
}

fn git_output<const N: usize>(repository: &Path, args: [&str; N]) -> AnyResult<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(Box::new(io::Error::other(format!(
            "git command failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))));
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

fn leantoken_source_identity() -> (Option<String>, Option<bool>) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let revision = git_output(root, ["rev-parse", "HEAD"]).ok();
    let dirty = revision.as_ref().and_then(|_| {
        git_output(root, ["status", "--porcelain", "--untracked-files=all"])
            .ok()
            .map(|status| !status.is_empty())
    });
    (revision, dirty)
}

fn corpus_report(
    args: &Args,
    corpus: &PreparedCorpus,
    files: &[DiscoveredFile],
    hot_set: usize,
) -> CorpusReport {
    let total_bytes = files.iter().map(|file| file.size_bytes).sum::<u64>();
    let mut extensions = BTreeMap::new();
    let mut max_directory_depth = 0usize;
    for file in files {
        let path = Path::new(&file.relative_path);
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .filter(|extension| !extension.is_empty())
            .unwrap_or("<none>")
            .to_ascii_lowercase();
        *extensions.entry(extension).or_insert(0) += 1;
        max_directory_depth = max_directory_depth.max(path.components().count().saturating_sub(1));
    }
    CorpusReport {
        source_kind: corpus.source_kind,
        source_repository: corpus.source_repository.clone(),
        revision: corpus.revision.clone(),
        files: files.len(),
        total_bytes,
        mean_file_bytes: total_bytes as f64 / files.len() as f64,
        max_directory_depth,
        extensions,
        synthetic_file_bytes: args.repository.is_none().then_some(args.file_bytes),
        indexing_iterations: args.iterations,
        read_samples: args.read_samples,
        hot_set_files: hot_set,
    }
}

fn measure_changed_indexing<F>(
    iterations: usize,
    path: &Path,
    mut reconcile: F,
    files_seen: usize,
    measurement: &str,
) -> AnyResult<IndexMeasurement>
where
    F: FnMut() -> leantoken::Result<IndexResponse>,
{
    let mut durations = Vec::with_capacity(iterations);
    for iteration in 0..iterations {
        let mut file = OpenOptions::new().append(true).open(path)?;
        writeln!(file, "// profile mutation {iteration}")?;
        drop(file);

        let start = Instant::now();
        let response = reconcile()?;
        durations.push(start.elapsed());
        require_index_counts(&response, files_seen, 1, measurement)?;
    }
    Ok(IndexMeasurement {
        timing: TimingStats::from_durations(durations),
        files_seen_per_sample: files_seen,
        files_indexed_per_sample: 1,
        files_removed_per_sample: 0,
    })
}

fn measure_lifecycle_indexing<S, C, R>(
    iterations: usize,
    mut setup: S,
    mut reconcile: C,
    mut restore: R,
    expected: ExpectedIndexCounts,
    measurement: &str,
) -> AnyResult<IndexMeasurement>
where
    S: FnMut(usize) -> io::Result<()>,
    C: FnMut() -> leantoken::Result<IndexResponse>,
    R: FnMut() -> AnyResult<()>,
{
    let mut durations = Vec::with_capacity(iterations);
    for iteration in 0..iterations {
        setup(iteration)?;
        let start = Instant::now();
        let response = reconcile()?;
        durations.push(start.elapsed());
        require_index_counts(&response, expected.seen, expected.indexed, measurement)?;
        if response.files_removed != expected.removed {
            return Err(Box::new(io::Error::other(format!(
                "{measurement} expected files_removed={}; got files_removed={}",
                expected.removed, response.files_removed
            ))));
        }
        restore()?;
    }
    Ok(IndexMeasurement {
        timing: TimingStats::from_durations(durations),
        files_seen_per_sample: expected.seen,
        files_indexed_per_sample: expected.indexed,
        files_removed_per_sample: expected.removed,
    })
}

fn require_index_counts(
    response: &IndexResponse,
    files_seen: usize,
    files_indexed: usize,
    measurement: &str,
) -> AnyResult<()> {
    if response.files_seen != files_seen || response.files_indexed != files_indexed {
        return Err(Box::new(io::Error::other(format!(
            "{measurement} expected files_seen={files_seen}, files_indexed={files_indexed}; got files_seen={}, files_indexed={}",
            response.files_seen, response.files_indexed
        ))));
    }
    Ok(())
}

fn measure_file_reads(paths: &[PathBuf], samples: usize) -> AnyResult<ReadMeasurement> {
    let mut durations = Vec::with_capacity(samples);
    let mut total_bytes = 0u64;
    for sample in 0..samples {
        let start = Instant::now();
        let contents = fs::read(&paths[sample % paths.len()])?;
        durations.push(start.elapsed());
        total_bytes += contents.len() as u64;
        black_box(contents);
    }
    Ok(ReadMeasurement {
        timing: TimingStats::from_durations(durations),
        total_bytes,
        mean_bytes: total_bytes as f64 / samples as f64,
    })
}

fn measure_memory_copies(contents: &[Vec<u8>], samples: usize) -> ReadMeasurement {
    let mut durations = Vec::with_capacity(samples);
    let mut total_bytes = 0u64;
    for sample in 0..samples {
        let start = Instant::now();
        let copy = contents[sample % contents.len()].clone();
        durations.push(start.elapsed());
        total_bytes += copy.len() as u64;
        black_box(copy);
    }
    ReadMeasurement {
        timing: TimingStats::from_durations(durations),
        total_bytes,
        mean_bytes: total_bytes as f64 / samples as f64,
    }
}

fn create_corpus(root: &Path, files: usize, file_bytes: usize) -> AnyResult<Vec<PathBuf>> {
    let source = root.join("src");
    fs::create_dir_all(&source)?;
    let mut paths = Vec::with_capacity(files);
    for index in 0..files {
        let path = source.join(format!("file_{index:05}.rs"));
        fs::write(&path, synthetic_source(index, file_bytes))?;
        paths.push(path);
    }
    Ok(paths)
}

fn synthetic_source(index: usize, file_bytes: usize) -> String {
    let mut source = format!(
        "pub fn symbol_{index:05}() -> usize {{\n    {index}\n}}\n\n// deterministic profile padding: "
    );
    if source.len() < file_bytes {
        source.extend(std::iter::repeat_n('x', file_bytes - source.len()));
    }
    source.push('\n');
    source
}

impl TimingStats {
    fn from_durations(durations: Vec<Duration>) -> Self {
        assert!(!durations.is_empty());
        let mut micros = durations
            .into_iter()
            .map(|duration| duration.as_secs_f64() * 1_000_000.0)
            .collect::<Vec<_>>();
        micros.sort_by(f64::total_cmp);
        let total_us = micros.iter().sum::<f64>();
        Self {
            samples: micros.len(),
            total_ms: total_us / 1_000.0,
            mean_us: total_us / micros.len() as f64,
            min_us: micros[0],
            p50_us: percentile(&micros, 0.50),
            p95_us: percentile(&micros, 0.95),
            max_us: micros[micros.len() - 1],
        }
    }
}

fn percentile(sorted: &[f64], percentile: f64) -> f64 {
    let rank = (percentile * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_git(repository: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed with {status}");
    }

    #[test]
    fn percentile_uses_nearest_rank() {
        let samples = [1.0, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(percentile(&samples, 0.50), 3.0);
        assert_eq!(percentile(&samples, 0.95), 5.0);
    }

    #[test]
    fn small_profile_exercises_each_measurement() {
        let output = tempfile::tempdir().expect("output");
        let args = Args {
            repository: None,
            repository_label: None,
            files: 6,
            file_bytes: 256,
            iterations: 2,
            read_samples: 12,
            hot_set: 2,
            output: output.path().join("report.json"),
        };

        let report = run_profile(&args).expect("profile");

        assert_eq!(report.initial_index.response.files_indexed, 7);
        assert_eq!(report.full_noop.timing.samples, 2);
        assert_eq!(report.full_changed.files_indexed_per_sample, 1);
        assert_eq!(report.targeted_changed.files_seen_per_sample, 1);
        assert_eq!(report.create_delta.files_seen_per_sample, 1);
        assert_eq!(report.delete_targeted.files_removed_per_sample, 1);
        assert_eq!(report.rename_delta.files_removed_per_sample, 1);
        assert_eq!(report.ignore_change_delta.files_seen_per_sample, 1);
        assert_eq!(report.warm_hot_file_reads.timing.samples, 12);
        assert_eq!(report.memory_hot_file_copies.timing.samples, 12);
    }

    #[test]
    fn repository_profile_mutates_only_a_disposable_snapshot() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let repository = tempfile::tempdir().expect("repository");
        run_git(repository.path(), &["init", "--quiet"]);
        run_git(
            repository.path(),
            &["config", "user.email", "test@example.com"],
        );
        run_git(
            repository.path(),
            &["config", "user.name", "LeanToken Test"],
        );
        let first = repository.path().join("first.rs");
        let second = repository.path().join("second.py");
        fs::write(&first, "fn first() {}\n").expect("first source");
        fs::write(&second, "def second():\n    return 2\n").expect("second source");
        run_git(repository.path(), &["add", "-A"]);
        run_git(repository.path(), &["commit", "--quiet", "-m", "fixture"]);
        let revision = git_output(repository.path(), ["rev-parse", "HEAD"]).expect("revision");
        let original = fs::read_to_string(&first).expect("original source");
        let output = tempfile::tempdir().expect("output");
        let args = Args {
            repository: Some(repository.path().to_path_buf()),
            repository_label: None,
            files: 2,
            file_bytes: 128,
            iterations: 1,
            read_samples: 2,
            hot_set: 1,
            output: output.path().join("report.json"),
        };

        let report = run_profile(&args).expect("profile repository");

        assert_eq!(report.corpus.source_kind, "git_worktree_snapshot");
        assert_eq!(report.corpus.revision.as_deref(), Some(revision.as_str()));
        assert_eq!(report.corpus.files, 3);
        assert_eq!(report.full_noop.files_seen_per_sample, 3);
        assert_eq!(report.corpus.extensions.get("rs"), Some(&1));
        assert_eq!(report.corpus.extensions.get("py"), Some(&1));
        assert_eq!(
            fs::read_to_string(first).expect("source after profile"),
            original
        );
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(repository.path())
                .args(["status", "--porcelain"])
                .output()
                .expect("git status")
                .stdout
                .is_empty()
        );
    }
}
