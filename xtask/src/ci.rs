use blake3::Hasher;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

const TOPOLOGY_PATH: &str = "ci/test-topology.json";
const PLAN_SCHEMA_VERSION: u32 = 2;
const PLANNER_VERSION: &str = "ci-planner-v3";
const MAX_CHANGED_PATHS_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_CHANGED_PATHS: usize = 100_000;
const MAX_RECEIPT_BYTES: u64 = 64 * 1024;
const MAX_STRESS_REPETITIONS: usize = 100;
const MAX_MATRIX_ENTRIES: usize = 32;
const ALLOWED_RUNNERS: &[&str] = &["ubuntu-latest", "macos-latest", "windows-latest"];

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Event {
    PullRequest,
    MergeGroup,
    Push,
    Schedule,
    Manual,
}

impl Event {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "pull_request" | "pull-request" => Ok(Self::PullRequest),
            "merge_group" | "merge-group" => Ok(Self::MergeGroup),
            "push" | "main_push" => Ok(Self::Push),
            "schedule" => Ok(Self::Schedule),
            "manual" | "workflow_dispatch" => Ok(Self::Manual),
            _ => Err(format!("unknown CI event `{value}`")),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct PlannerInput {
    pub(crate) event: Event,
    #[serde(default)]
    pub(crate) base_revision: Option<String>,
    pub(crate) head_revision: String,
    #[serde(default)]
    pub(crate) source_revision: Option<String>,
    #[serde(default)]
    pub(crate) schedule: Option<String>,
    #[serde(default)]
    pub(crate) changed_paths: Vec<String>,
    #[serde(default)]
    pub(crate) full_run: bool,
    #[serde(default)]
    pub(crate) diagnostic: bool,
    #[serde(default)]
    pub(crate) fork: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct Topology {
    schema_version: u32,
    max_matrix_entries: usize,
    known_paths: Vec<String>,
    lanes: Vec<Lane>,
}

#[derive(Debug, Clone, Deserialize)]
struct Lane {
    id: String,
    owner: String,
    allowed_events: Vec<Event>,
    required_events: Vec<Event>,
    command: CommandClass,
    paths: Vec<String>,
    matrix: Vec<MatrixEntry>,
    #[serde(default)]
    schedule: Option<String>,
    #[serde(default)]
    depends_on: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CommandClass {
    Quality,
    SecretScan,
    RustQuality,
    Product,
    Contract,
    Examples,
    Coverage,
    DependencyAudit,
    ReleasePlan,
    Npm,
    Stress,
    Profile,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
struct MatrixEntry {
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    repetitions: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct JobSpec {
    pub(crate) lane: String,
    pub(crate) runner: String,
    pub(crate) command: CommandClass,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) repetitions: Option<usize>,
    pub(crate) source_revision: String,
    pub(crate) topology_digest: String,
    pub(crate) receipt_identity: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct LaneDecision {
    pub(crate) lane: String,
    pub(crate) reason: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct DependencyEdge {
    pub(crate) lane: String,
    pub(crate) depends_on: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct Plan {
    pub(crate) schema_version: u32,
    pub(crate) planner_version: String,
    pub(crate) topology_digest: String,
    pub(crate) event: Event,
    pub(crate) schedule: Option<String>,
    pub(crate) base_revision: Option<String>,
    pub(crate) head_revision: String,
    pub(crate) source_revision: String,
    pub(crate) selected_lanes: Vec<LaneDecision>,
    pub(crate) unselected_lanes: Vec<LaneDecision>,
    pub(crate) dependencies: Vec<DependencyEdge>,
    pub(crate) jobs: Vec<JobSpec>,
    pub(crate) fallback_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[allow(
    dead_code,
    reason = "receipt fields are the stable CI handoff contract"
)]
pub(crate) enum ReceiptStatus {
    Passed,
    IntentionallyUnselected,
    UnexpectedlySkipped,
    Failed,
    Cancelled,
    InvalidPlan,
    Missing,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[allow(
    dead_code,
    reason = "receipt fields are the stable CI handoff contract"
)]
pub(crate) struct LaneReceipt {
    pub(crate) schema_version: u32,
    pub(crate) planner_version: String,
    pub(crate) topology_digest: String,
    pub(crate) source_revision: String,
    pub(crate) lane: String,
    pub(crate) runner: String,
    pub(crate) command: CommandClass,
    pub(crate) receipt_identity: String,
    pub(crate) status: ReceiptStatus,
}

#[allow(
    dead_code,
    reason = "receipt classification is consumed by the provider aggregator"
)]
pub(crate) fn classify_receipt(
    plan: &Plan,
    lane: &str,
    runner: &str,
    receipt: Option<&LaneReceipt>,
) -> ReceiptStatus {
    let selected = plan
        .jobs
        .iter()
        .find(|job| job.lane == lane && job.runner == runner);
    let Some(selected) = selected else {
        return if receipt.is_none() {
            ReceiptStatus::IntentionallyUnselected
        } else {
            ReceiptStatus::InvalidPlan
        };
    };
    let Some(receipt) = receipt else {
        return ReceiptStatus::Missing;
    };
    if receipt.lane != selected.lane
        || receipt.runner != selected.runner
        || receipt.command != selected.command
        || receipt.receipt_identity != selected.receipt_identity
        || receipt.schema_version != plan.schema_version
        || receipt.planner_version != plan.planner_version
        || receipt.topology_digest != plan.topology_digest
        || receipt.source_revision != plan.source_revision
    {
        return ReceiptStatus::InvalidPlan;
    }
    receipt.status.clone()
}

pub(crate) fn run(root: &Path, args: Vec<String>) -> Result<(), String> {
    let Some(command) = args.first().map(String::as_str) else {
        return Err(usage());
    };
    match command {
        "plan" => run_plan(root, &args[1..]),
        "validate-plan" => run_validate_plan(root, &args[1..]),
        "validate-receipts" => run_validate_receipts(root, &args[1..]),
        _ => Err(usage()),
    }
}

pub(crate) fn check_topology(root: &Path) -> Result<(), String> {
    let topology = read_topology(root)?;
    validate_topology_structure(&topology)?;
    check_tracked_path_ownership(root, &topology)?;
    Ok(())
}

fn validate_topology_structure(topology: &Topology) -> Result<(), String> {
    if topology.schema_version != PLAN_SCHEMA_VERSION
        || topology.max_matrix_entries == 0
        || topology.max_matrix_entries > MAX_MATRIX_ENTRIES
    {
        return Err("topology schema or matrix bound is invalid".to_owned());
    }
    let lanes = topology
        .lanes
        .iter()
        .map(|lane| lane.id.as_str())
        .collect::<BTreeSet<_>>();
    if lanes.len() != topology.lanes.len() || lanes.is_empty() {
        return Err("topology lane identifiers must be unique and non-empty".to_owned());
    }
    let mut matrix_count = 0;
    for lane in &topology.lanes {
        if lane.owner.trim().is_empty() || lane.allowed_events.is_empty() || lane.matrix.is_empty()
        {
            return Err(format!(
                "lane {} has no owner, allowed events, or matrix",
                lane.id
            ));
        }
        let allowed_events = lane.allowed_events.iter().copied().collect::<BTreeSet<_>>();
        let required_events = lane
            .required_events
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let matrix_names = lane
            .matrix
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<BTreeSet<_>>();
        if allowed_events.len() != lane.allowed_events.len()
            || required_events.len() != lane.required_events.len()
            || !required_events.is_subset(&allowed_events)
            || matrix_names.len() != lane.matrix.len()
            || lane
                .matrix
                .iter()
                .any(|entry| !valid_matrix_entry(lane.command, entry))
            || lane
                .depends_on
                .iter()
                .any(|dependency| !lanes.contains(dependency.as_str()))
        {
            return Err(format!(
                "lane {} has invalid event eligibility, matrix entries, or dependencies",
                lane.id
            ));
        }
        matrix_count += lane.matrix.len();
    }
    if matrix_count > topology.max_matrix_entries {
        return Err(format!(
            "topology has {matrix_count} matrix entries, limit is {}",
            topology.max_matrix_entries
        ));
    }
    ensure_acyclic(topology)?;
    Ok(())
}

fn valid_matrix_entry(command: CommandClass, entry: &MatrixEntry) -> bool {
    ALLOWED_RUNNERS.contains(&entry.name.as_str())
        && match (command, entry.repetitions) {
            (CommandClass::Stress, Some(1..=MAX_STRESS_REPETITIONS)) => true,
            (CommandClass::Stress, _) => false,
            (_, None) => true,
            (_, Some(_)) => false,
        }
}

fn check_tracked_path_ownership(root: &Path, topology: &Topology) -> Result<(), String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z"])
        .output()
        .map_err(|error| format!("could not enumerate tracked files: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "could not enumerate tracked files: git exited with {}",
            output.status
        ));
    }

    for bytes in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let path = std::str::from_utf8(bytes)
            .map_err(|_| "tracked paths must be valid UTF-8 for CI ownership".to_owned())?;
        if !topology
            .known_paths
            .iter()
            .any(|pattern| path_matches(path, pattern))
        {
            return Err(format!("tracked path `{path}` is absent from known_paths"));
        }
        if !topology
            .lanes
            .iter()
            .any(|lane| lane.paths.iter().any(|pattern| path_matches(path, pattern)))
        {
            return Err(format!("tracked path `{path}` has no explicit lane owner"));
        }
    }
    Ok(())
}

fn run_plan(root: &Path, args: &[String]) -> Result<(), String> {
    let (input, output, dry_run) = parse_plan_args(root, args)?;
    let plan = build_plan(root, input)?;
    validate_plan(root, &plan)?;
    let rendered = serde_json::to_string_pretty(&plan).map_err(|error| error.to_string())?;
    if let Some(ref output) = output {
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::write(output, format!("{rendered}\n")).map_err(|error| error.to_string())?;
    }
    if dry_run || output.is_none() {
        println!("{rendered}");
    } else if let Some(output) = output.as_ref() {
        println!(
            "CI plan written to {} ({} selected, {} intentionally unselected)",
            output.display(),
            plan.selected_lanes.len(),
            plan.unselected_lanes.len()
        );
    }
    Ok(())
}

fn run_validate_plan(root: &Path, args: &[String]) -> Result<(), String> {
    if args.len() != 2 || args[0] != "--input" {
        return Err("`cargo xtask ci validate-plan` requires --input <plan.json>".to_owned());
    }
    let plan: Plan = read_json(Path::new(&args[1]))?;
    validate_plan(root, &plan)?;
    println!("CI plan: valid");
    Ok(())
}

fn run_validate_receipts(root: &Path, args: &[String]) -> Result<(), String> {
    let plan_path = option_value(args, "--plan")?.ok_or("--plan is required")?;
    let receipts_path = option_value(args, "--receipts")?.ok_or("--receipts is required")?;
    if args.len() != 4 {
        return Err(
            "`cargo xtask ci validate-receipts` requires --plan <plan.json> --receipts <directory>"
                .to_owned(),
        );
    }
    let plan: Plan = read_json(Path::new(&plan_path))?;
    validate_plan(root, &plan)?;
    validate_receipts(&plan, Path::new(&receipts_path))?;
    println!("CI receipts: valid ({} jobs)", plan.jobs.len());
    Ok(())
}

fn validate_receipts(plan: &Plan, directory: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(directory)
        .map_err(|error| format!("could not inspect receipt directory: {error}"))?;
    if !metadata.file_type().is_dir() {
        return Err("receipt directory must be a real directory, not a symlink".to_owned());
    }
    let canonical_directory = directory
        .canonicalize()
        .map_err(|error| format!("could not resolve receipt directory: {error}"))?;
    let mut receipts = BTreeMap::<String, LaneReceipt>::new();
    for entry in fs::read_dir(directory)
        .map_err(|error| format!("could not read receipt directory: {error}"))?
    {
        let entry = entry.map_err(|error| format!("could not read receipt entry: {error}"))?;
        if receipts.len() >= plan.jobs.len() {
            return Err(
                "receipt directory contains more entries than the executable plan".to_owned(),
            );
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("could not inspect receipt entry: {error}"))?;
        if !metadata.file_type().is_file() || metadata.len() > MAX_RECEIPT_BYTES {
            return Err("receipt entries must be bounded regular files".to_owned());
        }
        let canonical = path
            .canonicalize()
            .map_err(|error| format!("could not resolve receipt entry: {error}"))?;
        if !canonical.starts_with(&canonical_directory) {
            return Err("receipt entry resolves outside the receipt directory".to_owned());
        }
        let receipt: LaneReceipt = read_json(&canonical)?;
        if receipts
            .insert(receipt.receipt_identity.clone(), receipt)
            .is_some()
        {
            return Err("receipt identities must be unique".to_owned());
        }
    }

    for job in &plan.jobs {
        let status = classify_receipt(
            plan,
            &job.lane,
            &job.runner,
            receipts.get(&job.receipt_identity),
        );
        if status != ReceiptStatus::Passed {
            return Err(format!(
                "job {} on {} has receipt status {:?}",
                job.lane, job.runner, status
            ));
        }
    }
    if receipts.keys().any(|identity| {
        !plan
            .jobs
            .iter()
            .any(|job| &job.receipt_identity == identity)
    }) {
        return Err("receipt directory contains an identity absent from the plan".to_owned());
    }
    Ok(())
}

fn parse_plan_args(
    root: &Path,
    args: &[String],
) -> Result<(PlannerInput, Option<PathBuf>, bool), String> {
    if let Some(index) = args.iter().position(|arg| arg == "--input") {
        let path = args.get(index + 1).ok_or("--input requires a path")?;
        let input: PlannerInput = read_json(Path::new(path))?;
        let output = option_value(args, "--output")?.map(PathBuf::from);
        let dry_run = args.iter().any(|arg| arg == "--dry-run");
        return Ok((input, output, dry_run));
    }
    let event = option_value(args, "--event")?
        .ok_or_else(|| "--event is required".to_owned())
        .and_then(|value| Event::parse(&value))?;
    let head_revision = option_value(args, "--head")?.ok_or("--head is required")?;
    let base_revision = option_value(args, "--base")?;
    let changed_paths = if let Some(path) = option_value(args, "--changed-paths-file")? {
        read_changed_paths(root, &path)?
    } else {
        args.windows(2)
            .filter(|pair| pair[0] == "--changed-path")
            .map(|pair| pair[1].clone())
            .collect()
    };
    Ok((
        PlannerInput {
            event,
            base_revision,
            head_revision,
            source_revision: option_value(args, "--source")?,
            schedule: option_value(args, "--schedule")?,
            changed_paths,
            full_run: args.iter().any(|arg| arg == "--full-run"),
            diagnostic: args.iter().any(|arg| arg == "--diagnostic"),
            fork: args.iter().any(|arg| arg == "--fork"),
        },
        option_value(args, "--output")?.map(PathBuf::from),
        args.iter().any(|arg| arg == "--dry-run"),
    ))
}

fn option_value(args: &[String], option: &str) -> Result<Option<String>, String> {
    let Some(index) = args.iter().position(|arg| arg == option) else {
        return Ok(None);
    };
    args.get(index + 1)
        .cloned()
        .map(Some)
        .ok_or_else(|| format!("{option} requires a value"))
}

fn build_plan(root: &Path, input: PlannerInput) -> Result<Plan, String> {
    if input.head_revision.trim().is_empty() {
        return Err("head revision must not be empty".to_owned());
    }
    let topology = read_validated_topology(root)?;
    validate_schedule(&topology, &input)?;
    let paths = normalize_paths(&input.changed_paths)?;
    let known = paths.iter().all(|path| {
        topology
            .known_paths
            .iter()
            .any(|pattern| path_matches(path, pattern))
    });
    let missing_base = matches!(input.event, Event::PullRequest | Event::MergeGroup)
        && input.base_revision.as_deref().is_none_or(str::is_empty);
    let fallback_reason = if !known {
        Some("one or more changed paths are outside the checked-in topology".to_owned())
    } else if missing_base {
        Some("base revision is unavailable; selecting all evidence conservatively".to_owned())
    } else if input.fork {
        Some("fork input is untrusted; selecting the conservative evidence set".to_owned())
    } else {
        None
    };
    let conservative = fallback_reason.is_some();
    let mut selected = BTreeMap::<String, String>::new();
    for lane in &topology.lanes {
        let runnable = lane_runnable(lane, input.event, input.schedule.as_deref());
        let required = runnable && lane.required_events.contains(&input.event);
        let changed = !lane.paths.is_empty()
            && paths
                .iter()
                .any(|path| lane.paths.iter().any(|pattern| path_matches(path, pattern)));
        if runnable
            && (required
                || input.full_run
                || input.diagnostic
                || (changed
                    && matches!(
                        input.event,
                        Event::PullRequest
                            | Event::MergeGroup
                            | Event::Push
                            | Event::Schedule
                            | Event::Manual
                    )))
        {
            let reason = if input.full_run {
                "full-run override adds this lane".to_owned()
            } else if input.diagnostic {
                "diagnostic override adds this lane".to_owned()
            } else if required {
                format!("required for {} validation", event_name(input.event))
            } else if conservative {
                "conservative fallback selects this lane".to_owned()
            } else {
                format!("changed path matches lane owner {}", lane.owner)
            };
            selected.insert(lane.id.clone(), reason);
        }
    }
    if conservative {
        for lane in &topology.lanes {
            if lane_runnable(lane, input.event, input.schedule.as_deref()) {
                selected
                    .entry(lane.id.clone())
                    .or_insert_with(|| "conservative fallback selects this lane".to_owned());
            }
        }
    }
    // Dependencies are monotonic: selecting a lane always selects its declared prerequisites.
    let mut changed = true;
    while changed {
        changed = false;
        for lane in &topology.lanes {
            if selected.contains_key(&lane.id) {
                for dependency in &lane.depends_on {
                    if !selected.contains_key(dependency) {
                        selected.insert(
                            dependency.clone(),
                            format!("dependency of selected lane {}", lane.id),
                        );
                        changed = true;
                    }
                }
            }
        }
    }
    let selected_lanes = topology
        .lanes
        .iter()
        .filter_map(|lane| {
            selected.get(&lane.id).map(|reason| LaneDecision {
                lane: lane.id.clone(),
                reason: reason.clone(),
            })
        })
        .collect::<Vec<_>>();
    let unselected_lanes = topology
        .lanes
        .iter()
        .filter(|lane| !selected.contains_key(&lane.id))
        .map(|lane| LaneDecision {
            lane: lane.id.clone(),
            reason: if lane.paths.is_empty() {
                "not selected for this event".to_owned()
            } else {
                "no changed path matches this owner on this event".to_owned()
            },
        })
        .collect::<Vec<_>>();
    let dependencies = topology
        .lanes
        .iter()
        .filter(|lane| selected.contains_key(&lane.id))
        .flat_map(|lane| {
            lane.depends_on.iter().map(|dependency| DependencyEdge {
                lane: lane.id.clone(),
                depends_on: dependency.clone(),
            })
        })
        .collect::<Vec<_>>();
    let topology_digest = topology_digest(root)?;
    let source_revision = input
        .source_revision
        .filter(|revision| !revision.trim().is_empty())
        .unwrap_or_else(|| input.head_revision.clone());
    let jobs = build_jobs(
        &topology,
        selected.keys().map(String::as_str),
        &source_revision,
        &topology_digest,
    );
    Ok(Plan {
        schema_version: PLAN_SCHEMA_VERSION,
        planner_version: PLANNER_VERSION.to_owned(),
        topology_digest,
        event: input.event,
        schedule: input.schedule,
        base_revision: input.base_revision,
        head_revision: input.head_revision.clone(),
        source_revision,
        selected_lanes,
        unselected_lanes,
        dependencies,
        jobs,
        fallback_reason,
    })
}

fn build_jobs<'a>(
    topology: &Topology,
    selected: impl IntoIterator<Item = &'a str>,
    source_revision: &str,
    topology_digest: &str,
) -> Vec<JobSpec> {
    let selected = selected.into_iter().collect::<BTreeSet<_>>();
    topology
        .lanes
        .iter()
        .filter(|lane| selected.contains(lane.id.as_str()))
        .flat_map(|lane| {
            lane.matrix.iter().map(|entry| {
                let mut hasher = Hasher::new();
                for field in [
                    "ci-lane-receipt-v1",
                    source_revision,
                    topology_digest,
                    lane.id.as_str(),
                    entry.name.as_str(),
                ] {
                    hasher.update(field.as_bytes());
                    hasher.update(&[0]);
                }
                JobSpec {
                    lane: lane.id.clone(),
                    runner: entry.name.clone(),
                    command: lane.command,
                    repetitions: entry.repetitions,
                    source_revision: source_revision.to_owned(),
                    topology_digest: topology_digest.to_owned(),
                    receipt_identity: hasher.finalize().to_hex().to_string(),
                }
            })
        })
        .collect()
}

fn validate_plan(root: &Path, plan: &Plan) -> Result<(), String> {
    let topology = read_validated_topology(root)?;
    if plan.schema_version != PLAN_SCHEMA_VERSION || plan.planner_version != PLANNER_VERSION {
        return Err("plan schema or planner version is unsupported".to_owned());
    }
    if plan.source_revision.trim().is_empty() || plan.head_revision.trim().is_empty() {
        return Err("plan source and head revisions must not be empty".to_owned());
    }
    if plan.topology_digest != topology_digest(root)? {
        return Err("plan topology digest does not match the checked-in topology".to_owned());
    }
    if plan.event == Event::Schedule {
        let schedule = plan
            .schedule
            .as_deref()
            .filter(|schedule| !schedule.trim().is_empty())
            .ok_or("scheduled plans require a cron identity")?;
        if !topology
            .lanes
            .iter()
            .filter_map(|lane| lane.schedule.as_deref())
            .any(|known| known == schedule)
        {
            return Err(format!("unknown schedule cron identity `{schedule}`"));
        }
    }
    let known = topology
        .lanes
        .iter()
        .map(|lane| lane.id.as_str())
        .collect::<BTreeSet<_>>();
    let selected = plan
        .selected_lanes
        .iter()
        .map(|decision| decision.lane.as_str())
        .collect::<BTreeSet<_>>();
    let unselected = plan
        .unselected_lanes
        .iter()
        .map(|decision| decision.lane.as_str())
        .collect::<BTreeSet<_>>();
    if selected.len() != plan.selected_lanes.len()
        || unselected.len() != plan.unselected_lanes.len()
        || selected.intersection(&unselected).next().is_some()
        || selected
            .union(&unselected)
            .copied()
            .collect::<BTreeSet<_>>()
            != known
    {
        return Err("selected and unselected lanes must partition topology exactly".to_owned());
    }
    if plan
        .selected_lanes
        .iter()
        .any(|decision| decision.reason.trim().is_empty())
    {
        return Err("selected lanes require human-readable reasons".to_owned());
    }
    if plan
        .fallback_reason
        .as_ref()
        .is_some_and(|reason| reason.trim().is_empty())
    {
        return Err("fallback reason must not be empty".to_owned());
    }
    for lane in &topology.lanes {
        let runnable = lane_runnable(lane, plan.event, plan.schedule.as_deref());
        if selected.contains(lane.id.as_str()) && !runnable {
            return Err(format!(
                "lane {} is not runnable for {} validation",
                lane.id,
                event_name(plan.event)
            ));
        }
        if runnable
            && lane.required_events.contains(&plan.event)
            && !selected.contains(lane.id.as_str())
        {
            return Err(format!(
                "lane {} is required for {} but is unselected",
                lane.id,
                event_name(plan.event)
            ));
        }
    }
    let expected_jobs = build_jobs(
        &topology,
        selected.iter().copied(),
        &plan.source_revision,
        &plan.topology_digest,
    );
    let receipt_identities = plan
        .jobs
        .iter()
        .map(|job| job.receipt_identity.as_str())
        .collect::<BTreeSet<_>>();
    if plan.jobs != expected_jobs || receipt_identities.len() != plan.jobs.len() {
        return Err("executable jobs do not match the canonical topology matrix".to_owned());
    }
    if plan.jobs.len() > topology.max_matrix_entries || plan.jobs.len() > MAX_MATRIX_ENTRIES {
        return Err(format!(
            "executable plan has {} jobs, limit is {}",
            plan.jobs.len(),
            topology.max_matrix_entries
        ));
    }
    let edges = plan
        .dependencies
        .iter()
        .map(|edge| (edge.lane.as_str(), edge.depends_on.as_str()))
        .collect::<BTreeSet<_>>();
    if edges.len() != plan.dependencies.len()
        || plan.dependencies.iter().any(|edge| {
            !known.contains(edge.lane.as_str()) || !known.contains(edge.depends_on.as_str())
        })
    {
        return Err("dependency edges must be unique and reference known lanes".to_owned());
    }
    for lane in &topology.lanes {
        if selected.contains(lane.id.as_str()) {
            for dependency in &lane.depends_on {
                if !selected.contains(dependency.as_str())
                    || !edges.contains(&(lane.id.as_str(), dependency.as_str()))
                {
                    return Err(format!(
                        "missing dependency edge {} -> {}",
                        lane.id, dependency
                    ));
                }
            }
        }
    }
    ensure_acyclic(&topology)?;
    Ok(())
}

fn ensure_acyclic(topology: &Topology) -> Result<(), String> {
    let mut incoming = topology
        .lanes
        .iter()
        .map(|lane| (lane.id.clone(), lane.depends_on.len()))
        .collect::<BTreeMap<_, _>>();
    let mut queue = topology
        .lanes
        .iter()
        .filter(|lane| lane.depends_on.is_empty())
        .map(|lane| lane.id.clone())
        .collect::<VecDeque<_>>();
    let mut visited = 0;
    while let Some(done) = queue.pop_front() {
        visited += 1;
        for lane in &topology.lanes {
            if lane.depends_on.iter().any(|dependency| dependency == &done) {
                let count = incoming
                    .get_mut(&lane.id)
                    .ok_or_else(|| format!("unknown lane {}", lane.id))?;
                *count -= 1;
                if *count == 0 {
                    queue.push_back(lane.id.clone());
                }
            }
        }
    }
    if visited == topology.lanes.len() {
        Ok(())
    } else {
        Err("lane dependency graph contains a cycle".to_owned())
    }
}

fn normalize_paths(paths: &[String]) -> Result<Vec<String>, String> {
    let mut normalized = BTreeSet::new();
    if paths.len() > MAX_CHANGED_PATHS {
        return Err(format!(
            "changed-path input contains {} entries, limit is {MAX_CHANGED_PATHS}",
            paths.len()
        ));
    }
    for path in paths {
        if path.is_empty()
            || path.starts_with('/')
            || path.contains('\\')
            || path
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err(format!("invalid repository-relative POSIX path `{path}`"));
        }
        normalized.insert(path.to_owned());
    }
    Ok(normalized.into_iter().collect())
}

fn read_changed_paths(root: &Path, path: &str) -> Result<Vec<String>, String> {
    let path = root.join(path);
    let size = fs::metadata(&path)
        .map_err(|error| error.to_string())?
        .len();
    if size > MAX_CHANGED_PATHS_FILE_BYTES {
        return Err(format!(
            "changed-path input is {size} bytes, limit is {MAX_CHANGED_PATHS_FILE_BYTES}"
        ));
    }
    let file = fs::File::open(path).map_err(|error| error.to_string())?;
    let mut contents = Vec::with_capacity(size as usize);
    file.take(MAX_CHANGED_PATHS_FILE_BYTES + 1)
        .read_to_end(&mut contents)
        .map_err(|error| error.to_string())?;
    if contents.len() as u64 > MAX_CHANGED_PATHS_FILE_BYTES {
        return Err("changed-path input exceeded its byte bound".to_owned());
    }
    Ok(String::from_utf8_lossy(&contents)
        .lines()
        .map(str::to_owned)
        .collect())
}

fn validate_schedule(topology: &Topology, input: &PlannerInput) -> Result<(), String> {
    if input.event != Event::Schedule {
        return Ok(());
    }
    let schedule = input
        .schedule
        .as_deref()
        .filter(|schedule| !schedule.trim().is_empty())
        .ok_or("scheduled plans require a cron identity")?;
    if topology
        .lanes
        .iter()
        .filter_map(|lane| lane.schedule.as_deref())
        .any(|known| known == schedule)
    {
        Ok(())
    } else {
        Err(format!("unknown schedule cron identity `{schedule}`"))
    }
}

fn lane_runnable(lane: &Lane, event: Event, schedule: Option<&str>) -> bool {
    if !lane.allowed_events.contains(&event) {
        return false;
    }
    match event {
        Event::PullRequest | Event::MergeGroup | Event::Push => lane.schedule.is_none(),
        Event::Schedule => lane.schedule.is_none() || lane.schedule.as_deref() == schedule,
        Event::Manual => true,
    }
}

fn path_matches(path: &str, pattern: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix("**/Cargo.toml") {
        return path.ends_with("Cargo.toml") && (prefix.is_empty() || path.starts_with(prefix));
    }
    if let Some(prefix) = pattern.strip_suffix('/') {
        return path.starts_with(prefix) && path.len() > prefix.len();
    }
    path == pattern
}

fn event_name(event: Event) -> &'static str {
    match event {
        Event::PullRequest => "pull request",
        Event::MergeGroup => "merge queue",
        Event::Push => "main",
        Event::Schedule => "scheduled",
        Event::Manual => "manual",
    }
}

fn read_topology(root: &Path) -> Result<Topology, String> {
    read_json(&root.join(TOPOLOGY_PATH))
}

fn read_validated_topology(root: &Path) -> Result<Topology, String> {
    let topology = read_topology(root)?;
    validate_topology_structure(&topology)?;
    Ok(topology)
}

fn topology_digest(root: &Path) -> Result<String, String> {
    let bytes = fs::read(root.join(TOPOLOGY_PATH)).map_err(|error| error.to_string())?;
    let mut hasher = Hasher::new();
    hasher.update(&bytes);
    Ok(hasher.finalize().to_hex().to_string())
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let contents = fs::read_to_string(path).map_err(|error| error.to_string())?;
    serde_json::from_str(&contents).map_err(|error| format!("{}: {error}", path.display()))
}

fn usage() -> String {
    "cargo xtask ci plan [--input <input.json> | --event <event> --head <sha>] [--output <plan.json>] [--dry-run] | validate-plan --input <plan.json> | validate-receipts --plan <plan.json> --receipts <directory>".to_owned()
}

#[cfg(test)]
mod tests {
    use super::{
        CommandClass, Event, LaneReceipt, MatrixEntry, PlannerInput, ReceiptStatus, build_plan,
        classify_receipt, normalize_paths, run, valid_matrix_entry, validate_plan,
        validate_receipts,
    };
    use crate::workspace_root;
    use std::fs;

    fn input(event: Event, paths: &[&str]) -> PlannerInput {
        PlannerInput {
            event,
            base_revision: Some("base-sha".to_owned()),
            head_revision: "head-sha".to_owned(),
            source_revision: None,
            schedule: None,
            changed_paths: paths.iter().map(|path| (*path).to_owned()).collect(),
            full_run: false,
            diagnostic: false,
            fork: false,
        }
    }

    fn selected_lanes(plan: &super::Plan) -> Vec<&str> {
        plan.selected_lanes
            .iter()
            .map(|decision| decision.lane.as_str())
            .collect()
    }

    fn job_matrix(plan: &super::Plan) -> Vec<(&str, &str)> {
        plan.jobs
            .iter()
            .map(|job| (job.lane.as_str(), job.runner.as_str()))
            .collect()
    }

    #[test]
    fn every_event_has_explicit_static_and_lifecycle_selection() {
        let root = workspace_root();
        for event in [
            Event::PullRequest,
            Event::MergeGroup,
            Event::Push,
            Event::Schedule,
            Event::Manual,
        ] {
            let mut value = input(event, &["src/config.rs"]);
            if event == Event::Schedule {
                value.schedule = Some("0 3 * * *".to_owned());
            }
            let plan = build_plan(&root, value).expect("plan");
            validate_plan(&root, &plan).expect("valid plan");
            assert!(
                plan.selected_lanes
                    .iter()
                    .any(|lane| lane.lane == "quality")
            );
            assert!(
                plan.selected_lanes
                    .iter()
                    .any(|lane| lane.lane == "secret-scan")
            );
        }
    }

    #[test]
    fn pull_request_policy_has_exact_owner_and_job_selection() {
        let cases = [
            (
                "src/config.rs",
                vec!["quality", "rust-quality", "secret-scan", "product-linux"],
            ),
            (
                "crates/test-suite/src/domains/retrieval.rs",
                vec!["quality", "rust-quality", "secret-scan", "product-linux"],
            ),
            (
                "tests/process/mcp_protocol.rs",
                vec!["quality", "rust-quality", "secret-scan", "product-linux"],
            ),
            (
                "crates/benchmarks/src/bin/indexing_profile.rs",
                vec!["quality", "rust-quality", "secret-scan"],
            ),
            ("docs/testing.md", vec!["quality", "secret-scan"]),
            (
                "npm/leantoken.cjs",
                vec!["quality", "secret-scan", "release-plan", "npm"],
            ),
            (
                "Cargo.lock",
                vec![
                    "quality",
                    "rust-quality",
                    "secret-scan",
                    "product-linux",
                    "dependency-audit",
                    "release-plan",
                ],
            ),
        ];
        for (path, expected) in cases {
            let plan = build_plan(&workspace_root(), input(Event::PullRequest, &[path]))
                .expect("pull-request plan");
            assert_eq!(selected_lanes(&plan), expected, "path: {path}");
            assert_eq!(
                job_matrix(&plan),
                expected
                    .iter()
                    .map(|lane| (*lane, "ubuntu-latest"))
                    .collect::<Vec<_>>(),
                "path: {path}"
            );
            assert!(plan.fallback_reason.is_none(), "path: {path}");
        }
    }

    #[test]
    fn merge_push_schedule_and_manual_policies_have_exact_jobs() {
        for event in [Event::MergeGroup, Event::Push] {
            let plan = build_plan(&workspace_root(), input(event, &["src/config.rs"]))
                .expect("merge or push plan");
            assert_eq!(
                selected_lanes(&plan),
                vec![
                    "quality",
                    "rust-quality",
                    "secret-scan",
                    "product-linux",
                    "product-macos",
                    "product-windows",
                    "contract",
                    "examples",
                    "npm",
                ]
            );
            assert_eq!(
                job_matrix(&plan),
                vec![
                    ("quality", "ubuntu-latest"),
                    ("rust-quality", "ubuntu-latest"),
                    ("secret-scan", "ubuntu-latest"),
                    ("product-linux", "ubuntu-latest"),
                    ("product-macos", "macos-latest"),
                    ("product-windows", "windows-latest"),
                    ("contract", "ubuntu-latest"),
                    ("contract", "macos-latest"),
                    ("contract", "windows-latest"),
                    ("examples", "ubuntu-latest"),
                    ("npm", "ubuntu-latest"),
                ]
            );
        }

        let mut nightly = input(Event::Schedule, &[]);
        nightly.schedule = Some("0 3 * * *".to_owned());
        let nightly = build_plan(&workspace_root(), nightly).expect("nightly plan");
        assert_eq!(
            selected_lanes(&nightly),
            vec![
                "quality",
                "rust-quality",
                "secret-scan",
                "product-linux",
                "product-macos",
                "product-windows",
                "contract",
                "examples",
                "coverage",
                "dependency-audit",
                "scheduled-stress",
            ]
        );
        assert_eq!(
            nightly
                .jobs
                .iter()
                .filter(|job| job.lane == "scheduled-stress")
                .map(|job| (job.runner.as_str(), job.repetitions))
                .collect::<Vec<_>>(),
            vec![
                ("ubuntu-latest", Some(100)),
                ("macos-latest", Some(25)),
                ("windows-latest", Some(25)),
            ]
        );

        let mut manual = input(Event::Manual, &[]);
        manual.full_run = true;
        let manual = build_plan(&workspace_root(), manual).expect("manual full plan");
        assert_eq!(manual.unselected_lanes.len(), 0);
        assert_eq!(manual.jobs.len(), 18);
    }

    #[test]
    fn full_run_is_additive_and_missing_base_fails_conservatively() {
        let mut full = input(Event::PullRequest, &["docs/testing.md"]);
        full.full_run = true;
        let full_plan = build_plan(&workspace_root(), full).expect("full plan");
        assert!(
            full_plan.unselected_lanes.iter().any(|lane| {
                lane.lane == "scheduled-stress" || lane.lane == "scheduled-profile"
            })
        );

        let mut missing = input(Event::PullRequest, &["docs/testing.md"]);
        missing.base_revision = None;
        let plan = build_plan(&workspace_root(), missing).expect("fallback plan");
        assert!(plan.fallback_reason.is_some());
        assert!(
            plan.unselected_lanes
                .iter()
                .any(|lane| lane.lane == "scheduled-stress")
        );
    }

    #[test]
    fn unknown_and_fork_inputs_select_every_runnable_lane() {
        for (path, fork) in [("new/unknown.file", false), ("src/config.rs", true)] {
            let mut value = input(Event::PullRequest, &[path]);
            value.fork = fork;
            let plan = build_plan(&workspace_root(), value).expect("fallback plan");
            assert!(plan.fallback_reason.is_some());
            assert!(
                plan.unselected_lanes
                    .iter()
                    .any(|lane| lane.lane == "scheduled-stress")
            );
            assert!(
                plan.unselected_lanes
                    .iter()
                    .any(|lane| lane.lane == "scheduled-profile")
            );
        }
    }

    #[test]
    fn schedule_selects_only_the_matching_lifecycle_lane() {
        let mut nightly = input(Event::Schedule, &[]);
        nightly.schedule = Some("0 3 * * *".to_owned());
        let nightly_plan = build_plan(&workspace_root(), nightly).expect("nightly plan");
        assert!(
            nightly_plan
                .selected_lanes
                .iter()
                .any(|lane| lane.lane == "scheduled-stress")
        );
        assert!(
            nightly_plan
                .unselected_lanes
                .iter()
                .any(|lane| lane.lane == "scheduled-profile")
        );

        let mut weekly = input(Event::Schedule, &[]);
        weekly.schedule = Some("0 4 * * 1".to_owned());
        let weekly_plan = build_plan(&workspace_root(), weekly).expect("weekly plan");
        assert!(
            weekly_plan
                .selected_lanes
                .iter()
                .any(|lane| lane.lane == "scheduled-profile")
        );
        assert!(
            weekly_plan
                .unselected_lanes
                .iter()
                .any(|lane| lane.lane == "scheduled-stress")
        );
    }

    #[test]
    fn schedule_requires_a_known_cron_identity() {
        let mut value = input(Event::Schedule, &[]);
        value.schedule = Some("0 5 * * *".to_owned());
        assert!(build_plan(&workspace_root(), value).is_err());
    }

    #[test]
    fn malformed_paths_and_noncanonical_jobs_are_rejected() {
        assert!(normalize_paths(&["/absolute".to_owned()]).is_err());
        assert!(normalize_paths(&["src\\config.rs".to_owned()]).is_err());
        let mut plan = build_plan(
            &workspace_root(),
            input(Event::PullRequest, &["src/config.rs"]),
        )
        .expect("plan");
        plan.jobs.push(plan.jobs[0].clone());
        assert!(validate_plan(&workspace_root(), &plan).is_err());
    }

    #[test]
    fn executable_matrix_rejects_untrusted_runners_and_unbounded_repetition() {
        let ubuntu = MatrixEntry {
            name: "ubuntu-latest".to_owned(),
            repetitions: None,
        };
        assert!(valid_matrix_entry(CommandClass::Product, &ubuntu));

        let arbitrary_runner = MatrixEntry {
            name: "self-hosted".to_owned(),
            repetitions: None,
        };
        assert!(!valid_matrix_entry(
            CommandClass::Product,
            &arbitrary_runner
        ));

        let excessive_stress = MatrixEntry {
            name: "ubuntu-latest".to_owned(),
            repetitions: Some(101),
        };
        assert!(!valid_matrix_entry(CommandClass::Stress, &excessive_stress));
        assert!(!valid_matrix_entry(
            CommandClass::Product,
            &excessive_stress
        ));
    }

    #[test]
    fn planner_command_rejects_an_untrusted_topology_before_writing_matrix_json() {
        let root = tempfile::tempdir().expect("planner fixture");
        fs::create_dir(root.path().join("ci")).expect("fixture CI directory");
        let topology =
            fs::read(workspace_root().join("ci/test-topology.json")).expect("checked topology");
        let mut topology: serde_json::Value =
            serde_json::from_slice(&topology).expect("topology JSON");
        topology["lanes"][0]["matrix"][0]["name"] = serde_json::json!("self-hosted");
        fs::write(
            root.path().join("ci/test-topology.json"),
            serde_json::to_vec(&topology).expect("fixture topology JSON"),
        )
        .expect("fixture topology");
        let output = root.path().join("plan.json");
        let error = run(
            root.path(),
            vec![
                "plan".to_owned(),
                "--event".to_owned(),
                "pull_request".to_owned(),
                "--head".to_owned(),
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                "--changed-path".to_owned(),
                "src/config.rs".to_owned(),
                "--output".to_owned(),
                output.display().to_string(),
            ],
        )
        .expect_err("untrusted runner reached plan output");
        assert!(error.contains("invalid event eligibility, matrix entries, or dependencies"));
        assert!(!output.exists());
    }

    #[test]
    fn malformed_plan_cannot_unselect_a_required_merge_group_lane() {
        let mut plan = build_plan(
            &workspace_root(),
            input(Event::MergeGroup, &["docs/testing.md"]),
        )
        .expect("plan");
        plan.selected_lanes
            .retain(|decision| decision.lane != "product-linux");
        plan.unselected_lanes.push(super::LaneDecision {
            lane: "product-linux".to_owned(),
            reason: "bad test plan".to_owned(),
        });
        assert!(validate_plan(&workspace_root(), &plan).is_err());
    }

    #[test]
    fn receipt_statuses_include_missing_and_cancellation() {
        assert_ne!(ReceiptStatus::Missing, ReceiptStatus::Passed);
        assert_ne!(ReceiptStatus::Cancelled, ReceiptStatus::Passed);
    }

    #[test]
    fn receipt_classification_fails_closed_for_missing_or_stale_results() {
        let plan = build_plan(
            &workspace_root(),
            input(Event::PullRequest, &["src/config.rs"]),
        )
        .expect("plan");
        assert_eq!(
            classify_receipt(&plan, "product-linux", "ubuntu-latest", None),
            ReceiptStatus::Missing
        );
        assert_eq!(
            classify_receipt(
                &build_plan(
                    &workspace_root(),
                    input(Event::PullRequest, &["scripts/benchmark.sh"])
                )
                .expect("documentation plan"),
                "product-windows",
                "windows-latest",
                None,
            ),
            ReceiptStatus::IntentionallyUnselected
        );
        let quality_job = plan
            .jobs
            .iter()
            .find(|job| job.lane == "quality")
            .expect("quality job");
        let receipt = LaneReceipt {
            schema_version: plan.schema_version,
            planner_version: plan.planner_version.clone(),
            topology_digest: "stale".to_owned(),
            source_revision: plan.source_revision.clone(),
            lane: "quality".to_owned(),
            runner: quality_job.runner.clone(),
            command: quality_job.command,
            receipt_identity: quality_job.receipt_identity.clone(),
            status: ReceiptStatus::Passed,
        };
        assert_eq!(
            classify_receipt(&plan, "quality", "ubuntu-latest", Some(&receipt)),
            ReceiptStatus::InvalidPlan
        );
    }

    #[test]
    fn receipt_directory_must_cover_every_exact_planned_job() {
        let plan = build_plan(
            &workspace_root(),
            input(Event::PullRequest, &["docs/testing.md"]),
        )
        .expect("plan");
        let directory = tempfile::tempdir().expect("receipt directory");
        for job in &plan.jobs {
            let receipt = LaneReceipt {
                schema_version: plan.schema_version,
                planner_version: plan.planner_version.clone(),
                topology_digest: plan.topology_digest.clone(),
                source_revision: plan.source_revision.clone(),
                lane: job.lane.clone(),
                runner: job.runner.clone(),
                command: job.command,
                receipt_identity: job.receipt_identity.clone(),
                status: ReceiptStatus::Passed,
            };
            fs::write(
                directory
                    .path()
                    .join(format!("receipt-{}.json", job.receipt_identity)),
                serde_json::to_vec(&receipt).expect("receipt JSON"),
            )
            .expect("write receipt");
        }
        validate_receipts(&plan, directory.path()).expect("complete receipt set");

        fs::remove_file(
            directory
                .path()
                .join(format!("receipt-{}.json", plan.jobs[0].receipt_identity)),
        )
        .expect("remove receipt");
        assert!(validate_receipts(&plan, directory.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn receipt_directory_rejects_symlink_entries() {
        use std::os::unix::fs::symlink;

        let plan = build_plan(
            &workspace_root(),
            input(Event::PullRequest, &["docs/testing.md"]),
        )
        .expect("plan");
        let directory = tempfile::tempdir().expect("receipt directory");
        let outside = tempfile::NamedTempFile::new().expect("outside receipt");
        symlink(outside.path(), directory.path().join("receipt.json")).expect("receipt symlink");

        assert!(validate_receipts(&plan, directory.path()).is_err());
    }

    #[test]
    fn workflow_consumes_planner_jobs_and_validates_receipts() {
        let workflow = fs::read_to_string(workspace_root().join(".github/workflows/ci.yml"))
            .expect("CI workflow");
        assert!(workflow.contains("fromJSON(needs.changes.outputs.job_matrix)"));
        assert!(workflow.contains("cargo xtask ci validate-receipts"));
        assert!(workflow.contains("status=unexpectedly_skipped"));
        assert!(workflow.contains("target/ci-command-completed"));
        assert!(workflow.contains("PLANNED_RUNNER: ${{ matrix.runner }}"));
        assert!(!workflow.contains("RUNNER_NAME: ${{ matrix.runner }}"));
        assert!(workflow.contains("Checkout complete history for secret scanning"));
        assert!(workflow.contains("fetch-depth: 0"));
        assert!(workflow.contains("scripts/ci-secret-scan-range.sh"));
        assert!(workflow.contains("gitleaks --redact --timeout=300 git"));
        assert!(!workflow.contains("gitleaks/gitleaks-action"));
        assert_eq!(workflow.matches("overwrite: true").count(), 5);
        assert!(workflow.contains(
            "matrix.command == 'rust-quality' || matrix.command == 'product' || matrix.command == 'stress' || matrix.command == 'profile'"
        ));
        assert!(!workflow.contains("os: [ubuntu-latest, macos-latest, windows-latest]"));
    }
}
