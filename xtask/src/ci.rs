use blake3::Hasher;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

const TOPOLOGY_PATH: &str = "ci/test-topology.json";
const PLAN_SCHEMA_VERSION: u32 = 1;
const PLANNER_VERSION: &str = "ci-planner-v1";

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
    required_events: Vec<Event>,
    paths: Vec<String>,
    matrix: Vec<MatrixEntry>,
    #[serde(default)]
    depends_on: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct MatrixEntry {
    name: String,
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
    pub(crate) base_revision: Option<String>,
    pub(crate) head_revision: String,
    pub(crate) source_revision: String,
    pub(crate) selected_lanes: Vec<LaneDecision>,
    pub(crate) unselected_lanes: Vec<LaneDecision>,
    pub(crate) dependencies: Vec<DependencyEdge>,
    pub(crate) matrices: BTreeMap<String, Vec<MatrixEntry>>,
    pub(crate) fallback_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
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
    pub(crate) status: ReceiptStatus,
}

#[allow(
    dead_code,
    reason = "receipt classification is consumed by the provider aggregator"
)]
pub(crate) fn classify_receipt(
    plan: &Plan,
    lane: &str,
    receipt: Option<&LaneReceipt>,
) -> ReceiptStatus {
    let selected = plan
        .selected_lanes
        .iter()
        .any(|decision| decision.lane == lane);
    let Some(receipt) = receipt else {
        return if selected {
            ReceiptStatus::Missing
        } else {
            ReceiptStatus::IntentionallyUnselected
        };
    };
    if receipt.lane != lane
        || receipt.schema_version != plan.schema_version
        || receipt.planner_version != plan.planner_version
        || receipt.topology_digest != plan.topology_digest
        || receipt.source_revision != plan.source_revision
    {
        return ReceiptStatus::InvalidPlan;
    }
    if selected {
        receipt.status.clone()
    } else {
        ReceiptStatus::InvalidPlan
    }
}

pub(crate) fn run(root: &Path, args: Vec<String>) -> Result<(), String> {
    let Some(command) = args.first().map(String::as_str) else {
        return Err(usage());
    };
    match command {
        "plan" => run_plan(root, &args[1..]),
        "validate-plan" => run_validate_plan(root, &args[1..]),
        _ => Err(usage()),
    }
}

pub(crate) fn check_topology(root: &Path) -> Result<(), String> {
    let topology = read_topology(root)?;
    if topology.schema_version != PLAN_SCHEMA_VERSION || topology.max_matrix_entries == 0 {
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
        if lane.owner.trim().is_empty() || lane.matrix.is_empty() {
            return Err(format!("lane {} has no owner or matrix", lane.id));
        }
        let matrix_names = lane
            .matrix
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<BTreeSet<_>>();
        if matrix_names.len() != lane.matrix.len()
            || lane
                .depends_on
                .iter()
                .any(|dependency| !lanes.contains(dependency.as_str()))
        {
            return Err(format!(
                "lane {} has duplicate matrix entries or unknown dependencies",
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
    ensure_acyclic(&topology)?;
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
        let contents = fs::read_to_string(root.join(path)).map_err(|error| error.to_string())?;
        contents.lines().map(str::to_owned).collect()
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
    let topology = read_topology(root)?;
    if topology.schema_version != PLAN_SCHEMA_VERSION {
        return Err(format!(
            "unsupported topology schema {}",
            topology.schema_version
        ));
    }
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
        let required = lane.required_events.contains(&input.event);
        let changed = !lane.paths.is_empty()
            && paths
                .iter()
                .any(|path| lane.paths.iter().any(|pattern| path_matches(path, pattern)));
        if required
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
                ))
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
            selected
                .entry(lane.id.clone())
                .or_insert_with(|| "conservative fallback selects this lane".to_owned());
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
    let matrices = topology
        .lanes
        .iter()
        .filter(|lane| selected.contains_key(&lane.id))
        .map(|lane| (lane.id.clone(), lane.matrix.clone()))
        .collect::<BTreeMap<_, _>>();
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
    Ok(Plan {
        schema_version: PLAN_SCHEMA_VERSION,
        planner_version: PLANNER_VERSION.to_owned(),
        topology_digest: topology_digest(root)?,
        event: input.event,
        base_revision: input.base_revision,
        head_revision: input.head_revision.clone(),
        source_revision: input.head_revision,
        selected_lanes,
        unselected_lanes,
        dependencies,
        matrices,
        fallback_reason,
    })
}

fn validate_plan(root: &Path, plan: &Plan) -> Result<(), String> {
    let topology = read_topology(root)?;
    if plan.schema_version != PLAN_SCHEMA_VERSION || plan.planner_version != PLANNER_VERSION {
        return Err("plan schema or planner version is unsupported".to_owned());
    }
    if plan.source_revision.trim().is_empty() || plan.head_revision.trim().is_empty() {
        return Err("plan source and head revisions must not be empty".to_owned());
    }
    if plan.topology_digest != topology_digest(root)? {
        return Err("plan topology digest does not match the checked-in topology".to_owned());
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
        if lane.required_events.contains(&plan.event) && !selected.contains(lane.id.as_str()) {
            return Err(format!(
                "lane {} is required for {} but is unselected",
                lane.id,
                event_name(plan.event)
            ));
        }
    }
    let mut matrix_count = 0;
    for lane in &topology.lanes {
        let Some(matrix) = plan.matrices.get(&lane.id) else {
            if selected.contains(lane.id.as_str()) {
                return Err(format!("selected lane {} has no matrix", lane.id));
            }
            continue;
        };
        if !selected.contains(lane.id.as_str()) || matrix != &lane.matrix {
            return Err(format!("matrix for lane {} is not canonical", lane.id));
        }
        let unique = matrix
            .iter()
            .map(|entry| &entry.name)
            .collect::<BTreeSet<_>>();
        if unique.len() != matrix.len() || matrix.is_empty() {
            return Err(format!("matrix for lane {} is duplicate or empty", lane.id));
        }
        matrix_count += matrix.len();
    }
    if matrix_count > topology.max_matrix_entries {
        return Err(format!(
            "matrix has {matrix_count} entries, limit is {}",
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
    for path in paths.iter().map(|path| path.trim()) {
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
    "cargo xtask ci plan [--input <input.json> | --event <event> --head <sha>] [--output <plan.json>] [--dry-run] | validate-plan --input <plan.json>".to_owned()
}

#[cfg(test)]
mod tests {
    use super::{
        Event, LaneReceipt, PlannerInput, ReceiptStatus, build_plan, classify_receipt,
        normalize_paths, validate_plan,
    };
    use crate::workspace_root;

    fn input(event: Event, paths: &[&str]) -> PlannerInput {
        PlannerInput {
            event,
            base_revision: Some("base-sha".to_owned()),
            head_revision: "head-sha".to_owned(),
            changed_paths: paths.iter().map(|path| (*path).to_owned()).collect(),
            full_run: false,
            diagnostic: false,
            fork: false,
        }
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
            let plan = build_plan(&root, input(event, &["src/config.rs"]).clone()).expect("plan");
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
    fn pull_request_selection_is_owner_specific() {
        let plan = build_plan(
            &workspace_root(),
            input(Event::PullRequest, &["docs/testing.md"]),
        )
        .expect("plan");
        assert!(
            plan.selected_lanes
                .iter()
                .all(|lane| lane.lane == "quality" || lane.lane == "secret-scan")
        );
        assert!(
            plan.unselected_lanes
                .iter()
                .any(|lane| lane.lane == "product-linux")
        );
        assert!(plan.fallback_reason.is_none());
    }

    #[test]
    fn representative_paths_select_each_pr_owned_lane() {
        let cases = [
            ("src/config.rs", "product-linux"),
            ("tests/benchmark_contract.rs", "contract"),
            ("examples/context_utilization.rs", "examples"),
            ("Cargo.lock", "dependency-audit"),
            ("npm/package.json", "npm"),
            ("dist-workspace.toml", "release-plan"),
        ];
        for (path, lane) in cases {
            let plan =
                build_plan(&workspace_root(), input(Event::PullRequest, &[path])).expect("plan");
            assert!(
                plan.selected_lanes
                    .iter()
                    .any(|decision| decision.lane == lane),
                "{path} did not select {lane}: {plan:?}"
            );
        }
    }

    #[test]
    fn full_run_is_additive_and_missing_base_fails_conservatively() {
        let mut full = input(Event::PullRequest, &["docs/testing.md"]);
        full.full_run = true;
        let full_plan = build_plan(&workspace_root(), full).expect("full plan");
        assert_eq!(full_plan.unselected_lanes.len(), 0);

        let mut missing = input(Event::PullRequest, &["docs/testing.md"]);
        missing.base_revision = None;
        let plan = build_plan(&workspace_root(), missing).expect("fallback plan");
        assert!(plan.fallback_reason.is_some());
        assert_eq!(plan.unselected_lanes.len(), 0);
    }

    #[test]
    fn unknown_and_fork_inputs_select_every_lane() {
        for (path, fork) in [("new/unknown.file", false), ("src/config.rs", true)] {
            let mut value = input(Event::PullRequest, &[path]);
            value.fork = fork;
            let plan = build_plan(&workspace_root(), value).expect("fallback plan");
            assert!(plan.fallback_reason.is_some());
            assert!(plan.unselected_lanes.is_empty());
        }
    }

    #[test]
    fn malformed_paths_and_duplicate_matrices_are_rejected() {
        assert!(normalize_paths(&["/absolute".to_owned()]).is_err());
        assert!(normalize_paths(&["src\\config.rs".to_owned()]).is_err());
        let mut plan = build_plan(
            &workspace_root(),
            input(Event::PullRequest, &["src/config.rs"]),
        )
        .expect("plan");
        plan.matrices
            .get_mut("quality")
            .unwrap()
            .push(super::MatrixEntry {
                name: "ubuntu-latest".to_owned(),
            });
        assert!(validate_plan(&workspace_root(), &plan).is_err());
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
            classify_receipt(&plan, "product-linux", None),
            ReceiptStatus::Missing
        );
        assert_eq!(
            classify_receipt(
                &build_plan(
                    &workspace_root(),
                    input(Event::PullRequest, &["docs/testing.md"])
                )
                .expect("documentation plan"),
                "product-windows",
                None,
            ),
            ReceiptStatus::IntentionallyUnselected
        );
        let receipt = LaneReceipt {
            schema_version: plan.schema_version,
            planner_version: plan.planner_version.clone(),
            topology_digest: "stale".to_owned(),
            source_revision: plan.source_revision.clone(),
            lane: "quality".to_owned(),
            status: ReceiptStatus::Passed,
        };
        assert_eq!(
            classify_receipt(&plan, "quality", Some(&receipt)),
            ReceiptStatus::InvalidPlan
        );
    }
}
