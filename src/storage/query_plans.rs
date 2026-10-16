//! Query-plan diagnostics have a separate owner from migrations and lifecycle tests.
//!
//! Policy: use the reviewed bundled SQLite build, leave QPSG disabled, and do not
//! run ANALYZE/PRAGMA optimize in either the product or this fixture. Plans are
//! checked as bounded work classes, never as complete human-text snapshots.
use super::hot_query::HotQuery;
use super::tests::sample_file;
use super::*;
use rusqlite::{StatementStatus, types::Value};

const IRRELEVANT_FILES: usize = 256;
const PAGE: usize = 7;

// A hot owner cannot be added or removed without a matching plan and behavior.
static HOT_QUERIES: &[&HotQuery] = &[
    &read_imports::AFFECTED_IMPORTERS,
    &read_syntax::CHUNKS_OVERLAPPING_BATCH,
    &read_syntax::RECEIPT_STRUCTURAL_MATCHES,
    &read_syntax::FIND_QUALIFIED_SYMBOL,
    &read_syntax::FIND_SYMBOL,
    &read_search::FIND_SYMBOLS_EXACT_BATCH,
    &read_search::SEARCH_SYMBOLS_TRIGRAM,
    &read_files::LIST_TREE_PATHS,
    &read_files::LIST_GLOB_PATHS,
    &read_meta::TOKEN_SAVINGS,
    &read_meta::SERVICE_FAILURES,
    &read_delta::READ_DELTA_EXACT,
    &read_delta::READ_DELTA_LATEST,
    &read_delta::READ_DELTA_PRUNE,
    &read_delta::READ_DELTA_OLDEST,
    &receipts::RECEIPT_LOOKUP,
    &receipts::RECEIPT_EVIDENCE,
    &receipts::RECEIPT_PRUNE,
    &receipts::RECEIPT_PRUNE_EXCEPT,
    &receipts::RECEIPT_OLDEST,
    &receipts::RECEIPT_OLDEST_EXCEPT,
    &query_receipts::QUERY_RECEIPT_LOOKUP,
    &query_receipts::QUERY_PARTITION,
    &projections::IMPORT_CANDIDATE_RESOLUTION,
    &read_files::REGEX_SCAN_FILES,
];

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Behavior {
    Imports,
    Chunks,
    Symbols,
    Paths,
    ReadDelta,
    Receipts,
    QueryReceipts,
    Accounting,
    Regex,
}

struct Case {
    query: &'static HotQuery,
    statement: Option<String>,
    parameters: Vec<Value>,
    behavior: Behavior,
    // Every SCAN must have an explicit owner and index class. Virtual scans
    // enumerate caller-owned JSON or FTS candidates, rather than base tables.
    scans: &'static [(&'static str, IndexClass)],
    order_sorts: usize,
    distinct_sorts: usize,
    subquery_scans: usize,
    max_rows: usize,
    max_fullscan_steps: usize,
}

impl Case {
    fn new(query: &'static HotQuery, parameters: Vec<Value>, behavior: Behavior) -> Self {
        Self {
            query,
            statement: None,
            parameters,
            behavior,
            scans: &[],
            order_sorts: 0,
            distinct_sorts: 0,
            subquery_scans: 0,
            max_rows: 1,
            max_fullscan_steps: 0,
        }
    }

    fn sql(&self) -> &str {
        self.statement.as_deref().unwrap_or(self.query.sql)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IndexClass {
    Named,
    PrimaryKey,
    Virtual,
    Automatic,
    None,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IndexedComparison {
    Equal,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct IndexedTerm {
    column: String,
    comparison: IndexedComparison,
}

#[derive(Clone, Debug)]
struct Access {
    owner: String,
    scan: bool,
    index_class: IndexClass,
    index: Option<String>,
    indexed_terms: Vec<IndexedTerm>,
    virtual_index: Option<String>,
    covering: bool,
}

impl Access {
    fn has_indexed_equality(&self, column: &str) -> bool {
        self.indexed_terms
            .iter()
            .any(|term| term.column == column && term.comparison == IndexedComparison::Equal)
    }

    fn uses_fts_match(&self) -> bool {
        // Bundled FTS5's xBestIndex encodes MATCH as M<column-number> in
        // idxStr. A virtual index without it can still scan the entire FTS
        // table while reporting zero SQLITE_STMTSTATUS_FULLSCAN_STEP rows.
        self.virtual_index
            .as_deref()
            .and_then(|index| index.split_once(':'))
            .is_some_and(|(_, constraints)| {
                constraints
                    .as_bytes()
                    .windows(2)
                    .any(|pair| pair[0] == b'M' && pair[1].is_ascii_digit())
            })
    }
}

#[derive(Clone, Debug)]
struct PlanNode {
    id: i64,
    parent: i64,
    detail: String,
    access: Option<Access>,
}

fn normalize_indexed_term(term: &str) -> Option<IndexedTerm> {
    // These describe index bounds printed by bundled SQLite, rather than
    // residual WHERE predicates. Preserve ranges instead of equating every
    // SEARCH through a named index with a selective equality lookup.
    [
        (">=", IndexedComparison::GreaterOrEqual),
        ("<=", IndexedComparison::LessOrEqual),
        ("=", IndexedComparison::Equal),
        (">", IndexedComparison::Greater),
        ("<", IndexedComparison::Less),
    ]
    .into_iter()
    .find_map(|(operator, comparison)| {
        let (column, value) = term.split_once(operator)?;
        let column = column
            .trim()
            .rsplit('.')
            .next()?
            .trim_matches(['"', '`', '[', ']']);
        (!column.is_empty() && !value.trim().is_empty()).then(|| IndexedTerm {
            column: column.to_owned(),
            comparison,
        })
    })
}

fn normalize_indexed_terms(detail: &str) -> Vec<IndexedTerm> {
    let Some((_, suffix)) = detail.split_once(" (") else {
        return Vec::new();
    };
    let Some((conditions, _)) = suffix.rsplit_once(')') else {
        return Vec::new();
    };
    let conditions = conditions.split_whitespace().collect::<Vec<_>>().join(" ");
    conditions
        .split(" AND ")
        .filter_map(normalize_indexed_term)
        .collect()
}

fn normalize_access(detail: &str) -> Option<Access> {
    let words = detail.split_whitespace().collect::<Vec<_>>();
    let scan = match words.first().copied()? {
        "SCAN" => true,
        "SEARCH" => false,
        _ => return None,
    };
    let owner_index = if words.get(1) == Some(&"TABLE") { 2 } else { 1 };
    let owner = (*words.get(owner_index)?).to_owned();
    let covering = words.contains(&"COVERING");
    let virtual_index = detail
        .strip_prefix("SCAN ")
        .or_else(|| detail.strip_prefix("SEARCH "))
        .and_then(|detail| detail.split_once("VIRTUAL TABLE INDEX "))
        .and_then(|(_, index)| index.split_whitespace().next())
        .map(str::to_owned);
    let (index_class, index) = if words.contains(&"AUTOMATIC") {
        (IndexClass::Automatic, None)
    } else if detail.contains("VIRTUAL TABLE INDEX") {
        (IndexClass::Virtual, None)
    } else if detail.contains("USING INTEGER PRIMARY KEY") {
        (IndexClass::PrimaryKey, None)
    } else if let Some(position) = words.iter().position(|word| *word == "INDEX") {
        (
            IndexClass::Named,
            words.get(position + 1).map(|name| (*name).to_owned()),
        )
    } else {
        (IndexClass::None, None)
    };
    Some(Access {
        owner,
        scan,
        index_class,
        index,
        indexed_terms: normalize_indexed_terms(detail),
        virtual_index,
        covering,
    })
}

fn explain(
    connection: &Connection,
    sql: &str,
    parameters: &[Value],
) -> rusqlite::Result<Vec<PlanNode>> {
    let mut statement = connection.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
    statement
        .query_map(rusqlite::params_from_iter(parameters), |row| {
            let detail: String = row.get(3)?;
            Ok(PlanNode {
                id: row.get(0)?,
                parent: row.get(1)?,
                access: normalize_access(&detail),
                detail,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
}

fn descends_from(plan: &[PlanNode], node: &PlanNode, ancestor: i64) -> bool {
    let mut parent = node.parent;
    // Malformed or cyclic parent relationships must not make diagnostics loop.
    for _ in 0..plan.len() {
        if parent == ancestor {
            return true;
        }
        let Some(node) = plan.iter().find(|node| node.id == parent) else {
            return false;
        };
        parent = node.parent;
    }
    false
}

fn qualified_symbol_branch_violations(plan: &[PlanNode], index: &str) -> Vec<String> {
    let mut errors = Vec::new();
    let unions = plan
        .iter()
        .filter(|node| node.detail == "MERGE (UNION ALL)")
        .collect::<Vec<_>>();
    let [union] = unions.as_slice() else {
        return vec!["qualified-symbol plan requires one merged UNION ALL".into()];
    };
    for name in ["LEFT", "RIGHT"] {
        let branches = plan
            .iter()
            .filter(|node| node.parent == union.id && node.detail == name)
            .collect::<Vec<_>>();
        let [branch] = branches.as_slice() else {
            errors.push(format!(
                "qualified-symbol UNION needs one distinct {name} branch"
            ));
            continue;
        };
        let lookups = plan
            .iter()
            .filter(|node| descends_from(plan, node, branch.id))
            .filter_map(|node| node.access.as_ref())
            .filter(|access| access.owner == "symbols")
            .collect::<Vec<_>>();
        if !matches!(lookups.as_slice(), [access] if !access.scan
            && access.index_class == IndexClass::Named && access.index.as_deref() == Some(index))
        {
            errors.push(format!("qualified-symbol {name} branch requires exactly one SEARCH symbols using {index}: {lookups:?}"));
        }
        if let [access] = lookups.as_slice()
            && !access.has_indexed_equality("name")
        {
            errors.push(format!(
                "qualified-symbol {name} branch requires indexed name equality: {access:?}"
            ));
        }
    }
    let index_lookups = plan
        .iter()
        .filter_map(|node| node.access.as_ref())
        .filter(|access| access.index.as_deref() == Some(index))
        .count();
    if index_lookups != 2 {
        errors.push(format!(
            "qualified-symbol UNION has extra or missing {index} accesses: {index_lookups}"
        ));
    }
    errors
}

fn relationship_violations(plan: &[PlanNode]) -> Vec<String> {
    let mut errors = Vec::new();
    let mut ids = HashSet::new();
    for node in plan {
        if !ids.insert(node.id) || node.id == node.parent {
            errors.push(format!("invalid node relationship: {node:?}"));
        }
    }
    // EQP omits its synthetic root (id zero); all other parents must exist.
    for node in plan {
        if node.parent != 0 && !ids.contains(&node.parent) {
            errors.push(format!("missing parent for node: {node:?}"));
        }
    }
    errors
}

fn subquery_scan(access: &Access) -> bool {
    access.scan && access.owner.starts_with("(subquery-") && access.index_class == IndexClass::None
}

fn access_violations(case: &Case, node: &PlanNode) -> Vec<String> {
    let Some(access) = &node.access else {
        return Vec::new();
    };
    let mut errors = Vec::new();
    if access.owner == "symbols_fts_trigram" && !access.uses_fts_match() {
        errors.push(format!("FTS MATCH constraint is required: {node:?}"));
    }
    if access.index_class == IndexClass::Named
        && !access
            .index
            .as_deref()
            .is_some_and(|index| case.query.contract.indexes.contains(&index))
    {
        errors.push(format!("unapproved schema index: {node:?}"));
    }
    if access.covering
        && access.index_class != IndexClass::Named
        && access.index_class != IndexClass::Automatic
    {
        errors.push(format!("unknown covering-index class: {node:?}"));
    }
    if access.index_class == IndexClass::Automatic {
        errors.push(format!("automatic index: {node:?}"));
    }
    if access.scan
        && !subquery_scan(access)
        && !case
            .scans
            .iter()
            .any(|(owner, index)| *owner == access.owner && *index == access.index_class)
    {
        errors.push(format!("unapproved scan: {node:?}"));
    }
    if !access.scan && access.index_class == IndexClass::None {
        errors.push(format!("unindexed search: {node:?}"));
    }
    errors
}

fn tree_order_sort_allowance(case: &Case, plan: &[PlanNode]) -> (usize, Vec<String>) {
    if case.query.contract.operation != "list_tree_paths" {
        return (case.order_sorts, Vec::new());
    }
    let accesses = plan
        .iter()
        .filter_map(|node| node.access.as_ref())
        .filter(|access| access.owner == "path_entries")
        .collect::<Vec<_>>();
    // Depth-selective requests can benefit from the depth index even though
    // ordering needs a top-N sort. A path-ordered access permits no such sort.
    // Neither choice proves scanned work is bounded by the returned page size.
    match accesses.as_slice() {
        [access] if access.index.as_deref() == Some("path_entries_depth_path_idx") => {
            (1, Vec::new())
        }
        [access] if access.index.as_deref() == Some("sqlite_autoindex_path_entries_1") => {
            (case.order_sorts, Vec::new())
        }
        _ => (
            case.order_sorts,
            vec!["tree requires one approved depth or ordered-path access".into()],
        ),
    }
}

fn temporary_work_violations(case: &Case, plan: &[PlanNode]) -> Vec<String> {
    let (allowed_order_sorts, mut errors) = tree_order_sort_allowance(case, plan);
    let mut order_sorts = 0;
    let mut distinct_sorts = 0;
    let mut subquery_scans = 0;
    for node in plan {
        if node.detail.contains("AUTOMATIC") || node.detail.starts_with("MATERIALIZE ") {
            errors.push(format!("unapproved index/materialization: {node:?}"));
        }
        if node.detail.starts_with("USE TEMP B-TREE") {
            if node.detail.ends_with("FOR ORDER BY") || node.detail.ends_with("PART OF ORDER BY") {
                order_sorts += 1;
            } else if node.detail.ends_with("FOR DISTINCT") {
                distinct_sorts += 1;
            } else {
                errors.push(format!("unapproved temporary work: {node:?}"));
            }
        }
        if node.access.as_ref().is_some_and(subquery_scan) {
            subquery_scans += 1;
        }
    }
    if order_sorts > allowed_order_sorts
        || distinct_sorts > case.distinct_sorts
        || subquery_scans > case.subquery_scans
    {
        errors.push(format!("temporary work exceeded contract: order={order_sorts}, distinct={distinct_sorts}, subqueries={subquery_scans}"));
    }
    errors
}

fn required_index_violations(case: &Case, plan: &[PlanNode]) -> Vec<String> {
    let mut errors = Vec::new();
    let tree = case.query.contract.operation == "list_tree_paths";
    let qualified_symbol = case.query.contract.operation == "find_qualified_symbol";
    if qualified_symbol {
        if let [index] = case.query.contract.indexes {
            errors.extend(qualified_symbol_branch_violations(plan, index));
        } else {
            errors.push("qualified-symbol contract must declare one name index".into());
        }
    }
    for index in case.query.contract.indexes {
        // The tree indexes are reviewed alternatives, not simultaneous access
        // requirements. Both still have to exist in the production schema.
        if tree || qualified_symbol {
            continue;
        }
        let count = plan
            .iter()
            .filter_map(|node| node.access.as_ref())
            .filter(|access| access.index.as_deref() == Some(index))
            .count();
        if count == 0 {
            errors.push(format!("expected an access using {index}, found none"));
        }
    }
    if case.query.contract.operation == "receipt_lookup"
        && !plan
            .iter()
            .filter_map(|node| node.access.as_ref())
            .any(|access| {
                access.owner == "retrieval_receipts" && access.index_class == IndexClass::PrimaryKey
            })
    {
        errors.push("receipt lookup must use its integer primary key".into());
    }
    if case.query.contract.operation == "search_symbols_trigram"
        && !plan
            .iter()
            .filter_map(|node| node.access.as_ref())
            .any(|access| access.owner == "symbols_fts_trigram" && access.uses_fts_match())
    {
        errors.push("FTS MATCH access is missing from the plan".into());
    }
    errors
}

fn violations(case: &Case, plan: &[PlanNode]) -> Vec<String> {
    let mut errors = relationship_violations(plan);
    errors.extend(temporary_work_violations(case, plan));
    for node in plan {
        errors.extend(access_violations(case, node));
    }
    errors.extend(required_index_violations(case, plan));
    errors
}

struct Fixture {
    _directory: tempfile::TempDir,
    storage: Storage,
    target_id: i64,
    receipt_id: String,
    query_receipt_id: String,
    delta_hash: String,
    query_record: crate::query_receipt::QueryReceiptRecord,
}

fn symbol(name: String, line: usize, parent: Option<&str>) -> SymbolInput {
    SymbolInput {
        name,
        kind: "function".into(),
        parent: parent.map(str::to_owned),
        signature: None,
        start_line: line,
        end_line: line,
        start_byte: 0,
        end_byte: 1,
    }
}

fn fixture() -> Fixture {
    let directory = tempfile::tempdir().expect("fixture directory");
    let storage = Storage::open(directory.path().join("index.sqlite")).expect("fixture storage");
    let mut files = Vec::new();
    for index in 0..IRRELEVANT_FILES + 3 {
        let path = match index {
            IRRELEVANT_FILES => "src/target.rs".to_owned(),
            n if n == IRRELEVANT_FILES + 1 => "src/importer-a.rs".to_owned(),
            n if n == IRRELEVANT_FILES + 2 => "src/importer-b.rs".to_owned(),
            _ => format!("src/noise-{index:04}.rs"),
        };
        let mut file = sample_file(&path, "a\nb\nc\nd\ne\nf\ng\nh\n");
        file.chunks = (0..4)
            .map(|chunk| ChunkInput {
                content: format!("chunk-{index}-{chunk}"),
                start_line: chunk * 2 + 1,
                end_line: chunk * 2 + 2,
                start_byte: chunk * 4,
                end_byte: chunk * 4 + 4,
                token_count: 1,
            })
            .collect();
        file.symbols = vec![
            symbol("common".into(), 1, Some("Services")),
            symbol(format!("noise_{index}"), 2, None),
            symbol(format!("noise_{index}"), 2, None),
        ];
        if path == "src/target.rs" {
            file.symbols.extend([
                symbol("selective".into(), 3, Some("Services")),
                symbol("Services.selective".into(), 3, None),
                symbol("unique".into(), 4, None),
            ]);
            file.symbols.extend(
                (0..=crate::receipt::MAX_REBASE_STRUCTURAL_CANDIDATES_PER_EVIDENCE)
                    .map(|n| symbol(format!("overflow_{n}"), 8, None)),
            );
        }
        file.imports = (0..4)
            .map(|edge| {
                let target = if path.starts_with("src/importer-") {
                    "src/target.rs".to_owned()
                } else {
                    format!("missing/{index}-{edge}.rs")
                };
                ImportInput {
                    raw_target: format!("import_{edge}"),
                    resolved_path: None,
                    candidate_paths: vec![target],
                    line: 1,
                }
            })
            .collect();
        files.push(file);
    }
    let baseline = storage.meta().expect("empty baseline");
    storage
        .publish_reconciliation_at(&baseline, "config", IndexingMode::Rebuild, |writer| {
            for file in files {
                writer.replace(file)?;
            }
            writer.repair_import_projections(|seed, _membership| {
                Ok(if seed.source_path.starts_with("src/importer-") {
                    vec!["absent.rs".into(), "src/target.rs".into()]
                } else {
                    vec![format!("missing/{}", seed.source_path)]
                })
            })
        })
        .expect("publish and repair adversarial fixture");
    let session = storage.begin_read().expect("fixture snapshot");
    let target_id = session
        .find_file("src/target.rs")
        .expect("target lookup")
        .expect("target")
        .id;
    drop(session);
    let evidence = crate::receipt::ReceiptEvidence::new(
        "src/target.rs",
        1,
        1,
        crate::text::hash("common"),
        None,
    );
    let mut receipt_id = String::new();
    for _ in 0..24 {
        receipt_id = storage
            .evaluate_receipt_at(None, 1, std::slice::from_ref(&evidence), true, 1_000)
            .expect("fixture receipt")
            .receipt_id;
    }
    let mut delta_hash = String::new();
    for index in 0..crate::read_delta::MAX_READ_DELTA_BASES {
        let base = crate::read_delta::ReadDeltaBase {
            content: format!("base-{index}"),
            generation: 1,
            target_start_line: 1,
            target_end_line: 1,
            returned_start_line: 1,
            returned_end_line: 1,
        };
        delta_hash = crate::text::hash(&base.content);
        assert!(
            storage
                .persist_read_delta_base_at("target", &delta_hash, &base, 1_000)
                .expect("fixture delta")
        );
    }
    let session = storage.begin_read().expect("partition snapshot");
    let predicate =
        crate::query_receipt::ExactQueryPredicate::from_request(&crate::model::SearchRequest {
            query: "absent".into(),
            mode: crate::model::SearchMode::Text,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(10),
            max_tokens: Some(1000),
            context_lines: Some(0),
            case_sensitive: true,
            all_occurrences: true,
            prefer_structural: false,
            receipt_id: None,
            query_receipt: None,
            cursor: None,
        })
        .expect("predicate");
    let mut query_record = crate::query_receipt::QueryReceiptRecord {
        repository_generation: 1,
        config_hash: "config".into(),
        predicate_blake3: predicate.digest().expect("predicate digest"),
        predicate,
        partition: session
            .exact_query_partition(|_| true, || Ok(()))
            .expect("partition"),
        match_count: 0,
        result_blake3: blake3::hash(b"result").to_hex().to_string(),
    };
    drop(session);
    let mut query_receipt_id = String::new();
    for index in 0..24 {
        query_record.result_blake3 = blake3::hash(format!("result-{index}").as_bytes())
            .to_hex()
            .to_string();
        query_receipt_id = storage
            .persist_query_receipt_at(&query_record, 1_000)
            .expect("fixture query receipt");
    }
    populate_accounting(&storage);
    Fixture {
        _directory: directory,
        storage,
        target_id,
        receipt_id,
        query_receipt_id,
        delta_hash,
        query_record,
    }
}

fn text(value: impl Into<String>) -> Value {
    Value::Text(value.into())
}
fn integer(value: i64) -> Value {
    Value::Integer(value)
}

fn cases(fixture: &Fixture) -> Vec<Case> {
    let file = fixture.target_id;
    let mut cases = Vec::new();
    let mut case = Case::new(
        &read_imports::AFFECTED_IMPORTERS,
        vec![text("[\"src/target.rs\",\"absent.rs\",\"src/target.rs\"]")],
        Behavior::Imports,
    );
    case.scans = &[("changed", IndexClass::Virtual)];
    case.distinct_sorts = 1;
    case.order_sorts = 1;
    case.max_rows = 2;
    cases.push(case);
    let mut case = Case::new(
        &projections::IMPORT_CANDIDATE_RESOLUTION,
        vec![text("[\"absent.rs\",\"src/target.rs\"]")],
        Behavior::Imports,
    );
    case.scans = &[("json_each", IndexClass::Virtual)];
    case.order_sorts = 1;
    cases.push(case);
    let mut case = Case::new(
        &read_syntax::CHUNKS_OVERLAPPING_BATCH,
        vec![text(
            serde_json::json!([
            {"file_id":file,"start_line":3,"end_line":4},
            {"file_id":-1,"start_line":1,"end_line":8},
            {"file_id":file,"start_line":3,"end_line":4}])
            .to_string(),
        )],
        Behavior::Chunks,
    );
    case.scans = &[("json_each", IndexClass::Virtual)];
    case.order_sorts = 1;
    case.max_rows = 2;
    cases.push(case);
    let mut case = Case::new(
        &read_search::FIND_SYMBOLS_EXACT_BATCH,
        vec![
            text("[\"selective\",\"absent\",\"common\",\"common\",\"noise_0\",\"noise_0\"]"),
            integer(PAGE as i64),
        ],
        Behavior::Symbols,
    );
    case.scans = &[("json_each", IndexClass::Virtual)];
    case.order_sorts = 2;
    case.max_rows = 5 + 2 * PAGE;
    cases.push(case);
    for (query, name, parent, leaf) in [
        (
            &read_syntax::FIND_SYMBOL,
            "unique",
            Value::Null,
            Value::Null,
        ),
        (
            &read_syntax::FIND_QUALIFIED_SYMBOL,
            "Services.selective",
            text("Services"),
            text("selective"),
        ),
    ] {
        let mut case = Case::new(
            query,
            vec![integer(file), text(name), parent, leaf],
            Behavior::Symbols,
        );
        case.order_sorts = 2;
        case.max_rows = 2;
        cases.push(case);
    }
    let mut case = Case::new(
        &read_search::SEARCH_SYMBOLS_TRIGRAM,
        vec![
            text("Services.common"),
            integer(1),
            integer(PAGE as i64),
            integer(0),
            text("\"common\""),
            integer(1),
            Value::Null,
        ],
        Behavior::Symbols,
    );
    case.scans = &[
        ("symbols_fts_trigram", IndexClass::Virtual),
        ("folded", IndexClass::Virtual),
    ];
    case.order_sorts = 1;
    case.max_rows = PAGE;
    cases.push(case);
    let mut case = Case::new(
        &read_syntax::RECEIPT_STRUCTURAL_MATCHES,
        vec![
            integer(file),
            integer(8),
            integer(8),
            integer((crate::receipt::MAX_REBASE_STRUCTURAL_CANDIDATES_PER_EVIDENCE + 1) as i64),
        ],
        Behavior::Receipts,
    );
    case.order_sorts = 2;
    case.subquery_scans = 1;
    case.max_rows = crate::receipt::MAX_REBASE_STRUCTURAL_CANDIDATES_PER_EVIDENCE + 1;
    cases.push(case);
    for (query, parameters) in [
        (
            &read_files::LIST_TREE_PATHS,
            vec![
                text("src"),
                integer(2),
                text("src/noise-0127.rs"),
                integer(PAGE as i64),
            ],
        ),
        (
            &read_files::LIST_GLOB_PATHS,
            vec![
                text("*.rs"),
                Value::Null,
                text("src/noise-0127.rs"),
                integer(PAGE as i64),
            ],
        ),
    ] {
        let mut case = Case::new(query, parameters, Behavior::Paths);
        case.scans = &[("path_entries", IndexClass::Named)];
        case.max_rows = PAGE;
        case.max_fullscan_steps = IRRELEVANT_FILES + 4;
        cases.push(case);
    }
    for (query, parameters) in [
        (
            &read_delta::READ_DELTA_EXACT,
            vec![text("target"), text(&fixture.delta_hash)],
        ),
        (&read_delta::READ_DELTA_LATEST, vec![text("target")]),
        (
            &read_delta::READ_DELTA_PRUNE,
            vec![integer(
                1_000 + crate::read_delta::READ_DELTA_BASE_TTL_MILLIS,
            )],
        ),
        (&read_delta::READ_DELTA_OLDEST, vec![]),
    ] {
        let mut case = Case::new(query, parameters, Behavior::ReadDelta);
        if query.contract.operation == "read_delta_oldest" {
            case.scans = &[("read_delta_bases", IndexClass::Named)];
        }
        cases.push(case);
    }
    for (query, parameters) in [
        (&receipts::RECEIPT_LOOKUP, vec![integer(1)]),
        (&receipts::RECEIPT_EVIDENCE, vec![integer(1)]),
        (
            &receipts::RECEIPT_PRUNE,
            vec![integer(1_000 + crate::receipt::RECEIPT_TTL_MILLIS)],
        ),
        (
            &receipts::RECEIPT_PRUNE_EXCEPT,
            vec![
                integer(1_000 + crate::receipt::RECEIPT_TTL_MILLIS),
                integer(24),
            ],
        ),
        (&receipts::RECEIPT_OLDEST, vec![]),
        (&receipts::RECEIPT_OLDEST_EXCEPT, vec![integer(1)]),
    ] {
        cases.push(Case::new(query, parameters, Behavior::Receipts));
    }
    let session = fixture.storage.begin_read().expect("receipt parameters");
    let identity: String = session
        .conn
        .query_row(
            "SELECT repository_identity FROM meta WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .expect("identity");
    drop(session);
    cases.push(Case::new(
        &query_receipts::QUERY_RECEIPT_LOOKUP,
        vec![
            text(identity),
            integer(1),
            text("config"),
            integer(crate::query_receipt::search_semantics_fingerprint() as i64),
            text(
                fixture
                    .query_record
                    .predicate
                    .serialized()
                    .expect("predicate"),
            ),
            text(&fixture.query_record.predicate_blake3),
            text(&fixture.query_record.partition.digest),
            integer(fixture.query_record.partition.file_count as i64),
            integer(0),
            text(&fixture.query_record.result_blake3),
        ],
        Behavior::QueryReceipts,
    ));
    let mut case = Case::new(
        &query_receipts::QUERY_PARTITION,
        vec![],
        Behavior::QueryReceipts,
    );
    case.scans = &[("files", IndexClass::Named)];
    case.max_rows = IRRELEVANT_FILES + 3;
    case.max_fullscan_steps = IRRELEVANT_FILES + 3;
    cases.push(case);
    for query in [&read_meta::TOKEN_SAVINGS, &read_meta::SERVICE_FAILURES] {
        let mut case = Case::new(query, vec![text("cl100k_base")], Behavior::Accounting);
        case.max_rows = 2;
        cases.push(case);
    }
    let includes = vec!["src/target.rs".into()];
    let (sql, params) = read_files::regex_scan_query(&includes, &[]);
    let mut case = Case::new(
        &read_files::REGEX_SCAN_FILES,
        std::iter::once(integer((IRRELEVANT_FILES + 4) as i64))
            .chain(params.into_iter().map(text))
            .collect(),
        Behavior::Regex,
    );
    case.statement = Some(sql);
    case.scans = &[("f", IndexClass::None)];
    case.max_fullscan_steps = IRRELEVANT_FILES + 3;
    cases.push(case);
    cases
}

fn populate_accounting(storage: &Storage) {
    let meta = ResponseMeta {
        repository_id: "repository".into(),
        repository_generation: 1,
        freshness: crate::model::Freshness::Current,
        index_scope: crate::model::IndexScopeMode::Full,
        index_scope_digest: None,
        source_tokens: 2,
        protocol_tokens: 3,
        path_and_metadata_tokens: 5,
        total_response_tokens: 10,
        tokenizer: "cl100k_base".into(),
        token_count_exact: true,
        receipt_id: None,
        receipt_suppressed_exact: 0,
        receipt_suppressed_overlap: 0,
        receipt_near_duplicates: 0,
        next_cursor: None,
    };
    for index in 0..33 {
        let tokenizer = if index == 32 {
            "cl100k_base".to_owned()
        } else {
            format!("irrelevant-{index}")
        };
        for operation in [
            TokenAccountingOperation::Read,
            TokenAccountingOperation::Search,
        ] {
            assert!(
                storage
                    .record_token_savings(
                        &tokenizer,
                        TokenSavingsObservation {
                            operation,
                            baseline_source_tokens: Some(10),
                            meta: &meta,
                            classification: TokenSavingsRequestClass::Useful,
                            expected_hash_not_modified: false,
                            expected_hash_suppressed_source_tokens: 0,
                        }
                    )
                    .expect("fixture savings")
            );
            assert!(
                storage
                    .record_service_failure(&tokenizer, operation, "invalid_input")
                    .expect("fixture failure")
            );
        }
    }
}

impl Behavior {
    fn verify(self, fixture: &Fixture) {
        let storage = &fixture.storage;
        let session = storage.begin_read().expect("behavior snapshot");
        match self {
            Self::Imports => verify_imports(&session),
            Self::Chunks => verify_chunks(&session, fixture),
            Self::Symbols => verify_symbols(&session, fixture),
            Self::Paths => verify_paths(&session),
            Self::ReadDelta => verify_read_delta(storage, session, fixture),
            Self::Receipts => verify_receipts(storage, session, fixture),
            Self::QueryReceipts => verify_query_receipts(storage, session, fixture),
            Self::Regex => verify_regex(&session),
            Self::Accounting => verify_accounting(&session),
        }
    }
}

fn verify_imports(session: &ReadSession) {
    assert!(
        session
            .affected_importers(&[])
            .expect("empty reverse lookup")
            .is_empty()
    );
    assert!(
        session
            .affected_importers(&["absent.rsx".into()])
            .expect("absent reverse lookup")
            .is_empty()
    );
    assert_eq!(
        session
            .affected_importers(&["src/target.rs".into(), "src/target.rs".into()])
            .expect("duplicate import edges"),
        ["src/importer-a.rs", "src/importer-b.rs"]
    );
    // Publication above repairs candidates through the production
    // resolver: missing first, existing second, four tied edges.
    for path in ["src/importer-a.rs", "src/importer-b.rs"] {
        let id = session
            .find_file(path)
            .expect("importer")
            .expect("existing importer")
            .id;
        let imports = session
            .get_imports_for_file(id, 10)
            .expect("repaired imports");
        assert_eq!(imports.len(), 4);
        assert!(
            imports
                .iter()
                .all(|import| import.resolved_path.as_deref() == Some("src/target.rs"))
        );
    }
}

fn verify_chunks(session: &ReadSession, fixture: &Fixture) {
    assert!(
        session
            .get_chunks_overlapping_batch(&[])
            .expect("empty ranges")
            .is_empty()
    );
    let groups = session
        .get_chunks_overlapping_batch(&[
            (fixture.target_id, 3, 4),
            (-1, 1, 8),
            (fixture.target_id, 3, 4),
            (fixture.target_id, 1, 8),
        ])
        .expect("batched ranges");
    assert_eq!(
        groups.iter().map(Vec::len).collect::<Vec<_>>(),
        [1, 0, 1, 4]
    );
    assert_eq!(groups[0][0].id, groups[2][0].id);
    assert!(
        groups[3]
            .windows(2)
            .all(|pair| pair[0].start_line < pair[1].start_line)
    );
}

fn verify_symbols(session: &ReadSession, fixture: &Fixture) {
    use crate::symbol_identity::SymbolResolution;
    let names = [
        "selective".into(),
        "absent".into(),
        "common".into(),
        "common".into(),
        "noise_0".into(),
        "noise_0".into(),
    ];
    let groups = session
        .find_symbols_exact_batch(&names, PAGE)
        .expect("bounded exact names");
    assert_eq!(
        groups.iter().map(Vec::len).collect::<Vec<_>>(),
        [1, 0, PAGE, PAGE, 2, 2]
    );
    assert_eq!(
        groups[2]
            .iter()
            .map(|hit| hit.symbol.id)
            .collect::<Vec<_>>(),
        groups[3]
            .iter()
            .map(|hit| hit.symbol.id)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        groups[4]
            .iter()
            .map(|hit| hit.symbol.id)
            .collect::<Vec<_>>(),
        groups[5]
            .iter()
            .map(|hit| hit.symbol.id)
            .collect::<Vec<_>>()
    );
    assert!(
        groups[4][0].symbol.id < groups[4][1].symbol.id,
        "tied byte offsets use stable ids"
    );
    assert_eq!(
        session
            .find_symbols_exact_batch(&["noise_0".into()], 1)
            .expect("tied result cap")[0]
            .len(),
        1
    );
    assert!(
        groups[2]
            .windows(2)
            .all(|pair| pair[0].path <= pair[1].path)
    );
    assert!(matches!(
        session
            .find_symbol(fixture.target_id, "unique")
            .expect("unique"),
        SymbolResolution::Unique(_)
    ));
    assert!(matches!(
        session
            .find_symbol(fixture.target_id, "Services.selective")
            .expect("two union branches"),
        SymbolResolution::Ambiguous
    ));
    assert!(matches!(
        session
            .find_symbol(fixture.target_id, "absent")
            .expect("missing"),
        SymbolResolution::NotFound
    ));
    assert!(matches!(
        session
            .find_symbol(fixture.target_id, "Services.absent")
            .expect("missing qualified"),
        SymbolResolution::NotFound
    ));
    let hits = session
        .search_symbols("Services.common", true, PAGE)
        .expect("high-cardinality FTS");
    assert_eq!(hits.len(), PAGE);
    assert!(hits.windows(2).all(|pair| pair[0].path <= pair[1].path));
    assert!(
        session
            .search_symbols("Services.absent", true, PAGE)
            .expect("absent FTS")
            .is_empty()
    );
    assert_eq!(
        session
            .search_symbols("Services.selective", true, PAGE)
            .expect("selective FTS")
            .len(),
        2
    );
}

fn verify_paths(session: &ReadSession) {
    let mut cursor = None;
    let mut all_paths = Vec::new();
    loop {
        let page = session
            .list_tree_paths("src", 1, cursor.as_deref(), PAGE)
            .expect("tree page");
        assert!(page.len() <= PAGE);
        if page.is_empty() {
            break;
        }
        assert!(
            page.iter()
                .all(|entry| entry.path == "src" || entry.path.starts_with("src/"))
        );
        cursor = page.last().map(|entry| entry.path.clone());
        all_paths.extend(page.into_iter().map(|entry| entry.path));
        assert!(
            all_paths.len() <= IRRELEVANT_FILES + 4,
            "pagination must terminate"
        );
    }
    assert_eq!(all_paths.len(), IRRELEVANT_FILES + 4);
    assert!(all_paths.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(
        session
            .list_tree_paths("absent", 1, None, PAGE)
            .expect("absent root")
            .is_empty()
    );
    assert_eq!(
        session
            .list_tree_paths("src", 0, None, PAGE)
            .expect("depth boundary")
            .len(),
        1
    );
    let mut cursor = None;
    let mut paths = Vec::new();
    loop {
        let page = session
            .list_glob_paths("*.rs", Some("src/*.rs"), cursor.as_deref(), PAGE)
            .expect("glob page");
        assert!(page.len() <= PAGE);
        if page.is_empty() {
            break;
        }
        cursor = page.last().map(|entry| entry.path.clone());
        paths.extend(page.into_iter().map(|entry| entry.path));
        assert!(
            paths.len() <= IRRELEVANT_FILES + 3,
            "glob pagination must terminate"
        );
    }
    assert_eq!(paths.len(), IRRELEVANT_FILES + 3);
    assert!(paths.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(
        session
            .list_glob_paths("src/target*", None, None, PAGE)
            .expect("selective prefix")
            .len(),
        1
    );
    assert!(
        session
            .list_glob_paths("*absent*", None, None, PAGE)
            .expect("absent wildcard")
            .is_empty()
    );
}

fn verify_read_delta(storage: &Storage, session: ReadSession, fixture: &Fixture) {
    drop(session);
    let exact = storage
        .read_delta_base_at("target", Some(&fixture.delta_hash), 1_001)
        .expect("exact base")
        .expect("base");
    let latest = storage
        .read_delta_base_at("target", None, 1_001)
        .expect("latest base")
        .expect("base");
    assert_eq!(exact, latest);
    assert!(
        storage
            .read_delta_base_at("absent", None, 1_001)
            .expect("absent base")
            .is_none()
    );
    let base = crate::read_delta::ReadDeltaBase {
        content: "newest".into(),
        generation: 2,
        target_start_line: 1,
        target_end_line: 1,
        returned_start_line: 1,
        returned_end_line: 1,
    };
    let hash = crate::text::hash(&base.content);
    assert!(
        storage
            .persist_read_delta_base_at("target", &hash, &base, 1_002)
            .expect("quota insertion")
    );
    assert!(
        storage
            .read_delta_base_at("target", Some(&crate::text::hash("base-0")), 1_002)
            .expect("oldest eviction")
            .is_none()
    );
    assert_eq!(
        storage
            .read_delta_base_at("target", None, 1_002)
            .expect("newest generation"),
        Some(base)
    );
    assert!(
        storage
            .read_delta_base_at(
                "target",
                None,
                1_002 + crate::read_delta::READ_DELTA_BASE_TTL_MILLIS
            )
            .expect("expiry boundary")
            .is_none()
    );
}

fn verify_receipts(storage: &Storage, session: ReadSession, fixture: &Fixture) {
    assert_eq!(
        session
            .receipt_structural_hash_matches(fixture.target_id, 1, 1, &crate::text::hash("common"))
            .expect("symbol branch"),
        Some(true)
    );
    assert_eq!(
        session
            .receipt_structural_hash_matches(
                fixture.target_id,
                1,
                1,
                &crate::text::hash("import_0")
            )
            .expect("import branch"),
        Some(true)
    );
    assert_eq!(
        session
            .receipt_structural_hash_matches(fixture.target_id, 8, 8, &crate::text::hash("absent"))
            .expect("candidate overflow"),
        None
    );
    assert_eq!(
        session
            .receipt_structural_hash_matches(-1, 1, 1, &crate::text::hash("absent"))
            .expect("missing structural evidence"),
        Some(false)
    );
    drop(session);
    let receipt = storage
        .read_receipt(&fixture.receipt_id, 1_001)
        .expect("real receipt and evidence lookup");
    assert_eq!(receipt.evidence.len(), 1);
    let source = storage
        .load_receipt_rebase_source_at(&fixture.receipt_id, 1_001)
        .expect("rebase source");
    let carried = storage
        .persist_rebased_receipt_at(&source, 1, &receipt.evidence, 1_002)
        .expect("retained-source pruning");
    let rebased = storage
        .read_receipt(&carried, 1_003)
        .expect("carried evidence");
    assert_eq!(rebased.evidence.len(), receipt.evidence.len());
    for (carried, original) in rebased.evidence.iter().zip(&receipt.evidence) {
        assert_eq!(
            (
                &carried.path,
                carried.start_line,
                carried.end_line,
                &carried.content_hash
            ),
            (
                &original.path,
                original.start_line,
                original.end_line,
                &original.content_hash
            ),
            "rebasing preserves exact evidence identity and ordinal order"
        );
        assert!(carried.exact_only());
        assert!(!original.exact_only());
        assert_eq!(carried.semantic_signature(), None);
    }
    let original = &receipt.evidence[0];
    let changed_overlap = crate::receipt::ReceiptEvidence::new(
        original.path.clone(),
        original.start_line,
        original.end_line,
        crate::text::hash("changed-overlap"),
        None,
    );
    let decisions = storage
        .evaluate_receipt_at(
            Some(&carried),
            1,
            &[original.clone(), changed_overlap],
            true,
            1_003,
        )
        .expect("exact-only carried evidence matching")
        .decisions;
    assert_eq!(
        decisions,
        [
            crate::receipt::ReceiptDecision::SuppressExact,
            crate::receipt::ReceiptDecision::Return
        ]
    );
    for _ in 0..crate::receipt::MAX_RECEIPTS {
        storage
            .evaluate_receipt_at(None, 1, &receipt.evidence, true, 1_004)
            .expect("receipt quota and LRU");
    }
    assert!(storage.read_receipt(&fixture.receipt_id, 1_005).is_err());
    storage
        .evaluate_receipt_at(
            None,
            1,
            &[],
            true,
            1_004 + crate::receipt::RECEIPT_TTL_MILLIS,
        )
        .expect("expiry pruning");
}

fn verify_query_receipts(storage: &Storage, session: ReadSession, fixture: &Fixture) {
    let loaded = session
        .load_query_receipt_at(&fixture.query_receipt_id, 1_001)
        .expect("real query receipt");
    assert_eq!(loaded.result_blake3, fixture.query_record.result_blake3);
    let partition = session
        .exact_query_partition(|path| path.starts_with("src/importer-"), || Ok(()))
        .expect("scoped partition");
    assert_eq!(partition.file_count, 2);
    let mut checked = 0;
    assert!(matches!(
        session.exact_query_partition(
            |_| true,
            || {
                checked += 1;
                if checked == 17 {
                    Err(Error::Cancelled)
                } else {
                    Ok(())
                }
            }
        ),
        Err(Error::Cancelled)
    ));
    assert_eq!(checked, 17);
    assert!(
        session
            .load_query_receipt_at(
                &fixture.query_receipt_id,
                1_000 + crate::query_receipt::QUERY_RECEIPT_TTL_MILLIS
            )
            .is_err()
    );
    drop(session);
    assert_eq!(
        storage
            .persist_query_receipt_at(&fixture.query_record, 1_001)
            .expect("exact receipt deduplication"),
        fixture.query_receipt_id
    );
}

fn verify_regex(session: &ReadSession) {
    let files = session
        .regex_scan_files(
            1,
            IRRELEVANT_FILES + 3,
            &["src/target.rs".into()],
            &[],
            |path| Ok(path == "src/target.rs"),
        )
        .expect("scope before retained-file cap");
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].0.path, "src/target.rs");
    assert_eq!(files[0].1, 4);
    assert!(
        matches!(session.regex_scan_files(PAGE, IRRELEVANT_FILES + 3, &[], &[], |_| Ok(true)),
        Err(Error::RetrievalLimitExceeded { kind: RetrievalLimitKind::RegexFullScanFiles, observed, limit }) if observed == PAGE + 1 && limit == PAGE)
    );
    assert!(matches!(
        session.regex_scan_files(PAGE, 1, &["*.rs".into()], &[], |_| Ok(false)),
        Err(Error::RetrievalLimitExceeded {
            kind: RetrievalLimitKind::RegexScopedRows,
            observed: 2,
            limit: 1
        })
    ));
    assert!(matches!(
        session.regex_scan_files(PAGE, IRRELEVANT_FILES + 3, &[], &[], |_| Err(
            Error::Cancelled
        )),
        Err(Error::Cancelled)
    ));
}

fn verify_accounting(session: &ReadSession) {
    let savings = session
        .token_savings("cl100k_base")
        .expect("selective tokenizer");
    assert_eq!(savings.len(), 2);
    assert!(savings.values().all(|record| record.tracked_requests == 1));
    assert!(
        session
            .token_savings("absent")
            .expect("absent tokenizer")
            .is_empty()
    );
    let failures = session
        .service_failures("cl100k_base")
        .expect("ordered failures");
    assert_eq!(failures.len(), 2);
    assert_eq!(
        failures
            .iter()
            .map(|record| record.operation.as_str())
            .collect::<Vec<_>>(),
        ["read", "search"]
    );
    assert!(failures.iter().all(|record| record.failed_requests == 1));
    assert!(
        session
            .service_failures("absent")
            .expect("absent failure tokenizer")
            .is_empty()
    );
}

fn qualified_symbol_ids(
    connection: &Connection,
    sql: &str,
    parameters: &[Value],
) -> rusqlite::Result<Vec<i64>> {
    let mut statement = connection.prepare(sql)?;
    statement
        .query_map(rusqlite::params_from_iter(parameters), |row| row.get(0))?
        .collect()
}

fn qualified_symbol_range_mutation_failures(connection: &Connection, case: &Case) -> Vec<String> {
    if case.query.contract.operation != "find_qualified_symbol" {
        return Vec::new();
    }
    let expected = match qualified_symbol_ids(connection, case.sql(), &case.parameters) {
        Ok(ids) => ids,
        Err(error) => {
            return vec![format!(
                "cannot collect qualified-symbol mutation baseline: {error}"
            )];
        }
    };
    let mut failures = Vec::new();
    for (branch, equality) in [
        ("LEFT", "name = ?2 COLLATE NOCASE"),
        ("RIGHT", "name = ?4 COLLATE NOCASE"),
    ] {
        // Keep the BINARY residual equality, output order, limit and bindings.
        // Answers stay the same while the NOCASE index bound becomes broad.
        let broad = case
            .sql()
            .replacen(equality, "name >= '' COLLATE NOCASE", 1);
        if broad == case.sql() {
            failures.push(format!(
                "negative {branch} mutation could not replace the indexed name equality"
            ));
            continue;
        }
        match qualified_symbol_ids(connection, &broad, &case.parameters) {
            Ok(ids) if ids == expected => {}
            result => failures.push(format!(
                "negative {branch} range mutation changed answers: {result:?}"
            )),
        }
        match explain(connection, &broad, &case.parameters) {
            Ok(plan) => {
                let expected =
                    format!("qualified-symbol {branch} branch requires indexed name equality");
                if !violations(case, &plan)
                    .iter()
                    .any(|error| error.starts_with(&expected))
                {
                    failures.push(format!("broad {branch} name-index range escaped the equality contract; raw plan={plan:#?}"));
                }
            }
            Err(error) => failures.push(format!(
                "negative {branch} range mutation did not produce an inspectable plan: {error}"
            )),
        }
    }
    failures
}

#[derive(Debug)]
struct ObservedWork {
    binds: usize,
    materialized: usize,
    fullscan: i32,
    vm_steps: i32,
    automatic: i32,
    sorts: i32,
}

fn observe_work(
    connection: &Connection,
    sql: &str,
    parameters: &[Value],
) -> rusqlite::Result<ObservedWork> {
    connection.execute_batch("SAVEPOINT query_plan_work")?;
    let observed = (|| {
        let mut statement = connection.prepare(sql)?;
        let binds = statement.parameter_count();
        let mut rows = statement.query(rusqlite::params_from_iter(parameters))?;
        let mut materialized = 0;
        while rows.next()?.is_some() {
            materialized += 1;
        }
        drop(rows);
        Ok(ObservedWork {
            binds,
            materialized,
            fullscan: statement.get_status(StatementStatus::FullscanStep),
            vm_steps: statement.get_status(StatementStatus::VmStep),
            automatic: statement.get_status(StatementStatus::AutoIndex),
            sorts: statement.get_status(StatementStatus::Sort),
        })
    })();
    let cleanup = connection.execute_batch("ROLLBACK TO query_plan_work; RELEASE query_plan_work");
    match (observed, cleanup) {
        (Err(error), _) | (_, Err(error)) => Err(error),
        (Ok(work), Ok(())) => Ok(work),
    }
}

#[test]
fn production_owned_plans_and_behavior_on_adversarial_cardinality() {
    let fixture = fixture();
    let cases = cases(&fixture);
    assert_eq!(
        cases
            .iter()
            .map(|case| case.query.contract.operation)
            .collect::<HashSet<_>>(),
        HOT_QUERIES
            .iter()
            .map(|query| query.contract.operation)
            .collect::<HashSet<_>>(),
        "every hot statement needs both a full plan and a storage-API behavior owner"
    );
    let mut behaviors = Vec::new();
    let mut failures = Vec::new();
    {
        let connection = fixture
            .storage
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(
            !connection
                .db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_QPSG)
                .expect("writer QPSG policy")
        );
        let source_id: String = connection
            .query_row("SELECT sqlite_source_id()", [], |row| row.get(0))
            .expect("SQLite build identity");
        let statistics: i64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name LIKE 'sqlite_stat%'",
                [],
                |row| row.get(0),
            )
            .expect("statistics policy");
        let statistics = usize::try_from(statistics).expect("nonnegative statistics count");
        assert_eq!(statistics, 0, "production and fixture do not run ANALYZE");
        eprintln!(
            "SQLite {} / {source_id}; QPSG=off; ANALYZE=never; irrelevant_files={IRRELEVANT_FILES}",
            rusqlite::version()
        );
        for case in &cases {
            if !behaviors.contains(&case.behavior) {
                behaviors.push(case.behavior);
            }
            let contract = &case.query.contract;
            let plan = match explain(&connection, case.sql(), &case.parameters) {
                Ok(plan) => plan,
                Err(error) => {
                    failures.push(format!(
                        "{}: cannot explain production statement: {error}",
                        contract.operation
                    ));
                    continue;
                }
            };
            let mut errors = violations(case, &plan);
            if contract.operation == "search_symbols_trigram" {
                let unconstrained = case.sql().replace(
                    "symbols_fts_trigram MATCH ?5",
                    "(?5 IS NULL OR ?5 IS NOT NULL)",
                );
                if unconstrained == case.sql() {
                    errors.push(
                        "negative mutation could not remove the production MATCH constraint".into(),
                    );
                } else {
                    match explain(&connection, &unconstrained, &case.parameters) {
                        Ok(regressed) => {
                            if !violations(case, &regressed)
                                .iter()
                                .any(|error| error.contains("FTS MATCH"))
                            {
                                errors.push(format!("unconstrained FTS scan escaped despite retaining bind shape and result limit; raw plan={regressed:#?}"));
                            }
                        }
                        Err(error) => errors.push(format!(
                            "negative FTS mutation did not produce an inspectable plan: {error}"
                        )),
                    }
                }
            }
            errors.extend(qualified_symbol_range_mutation_failures(&connection, case));
            for index in contract.indexes {
                let exists: bool = connection
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='index' AND name=?1)",
                        [index],
                        |row| row.get(0),
                    )
                    .expect("contract index ownership");
                if !exists {
                    errors.push(format!("removed schema index {index}"));
                }
            }
            match observe_work(&connection, case.sql(), &case.parameters) {
                Ok(work) => {
                    eprintln!(
                        "{}: rows={}, fullscan_steps={}, vm_steps={}, sorts={}, automatic_rows={}; raw plan={plan:#?}",
                        contract.operation,
                        work.materialized,
                        work.fullscan,
                        work.vm_steps,
                        work.sorts,
                        work.automatic
                    );
                    if work.binds != case.parameters.len()
                        || (case.statement.is_none() && work.binds != contract.parameters.len())
                    {
                        errors.push(format!(
                            "bind shape mismatch: declared={}, values={}, statement={}",
                            contract.parameters.len(),
                            case.parameters.len(),
                            work.binds
                        ));
                    }
                    if work.materialized > case.max_rows {
                        errors.push(format!(
                            "materialized {} > {}",
                            work.materialized, case.max_rows
                        ));
                    }
                    if work.fullscan < 0
                        || i64::from(work.fullscan)
                            > i64::try_from(case.max_fullscan_steps)
                                .expect("fixture scan bound fits i64")
                    {
                        errors.push(format!(
                            "fullscan steps {} > {} or counter overflow",
                            work.fullscan, case.max_fullscan_steps
                        ));
                    }
                    if !(0..250_000).contains(&work.vm_steps) {
                        errors.push(format!(
                            "excessive or overflowed VM work: {}",
                            work.vm_steps
                        ));
                    }
                    if work.automatic != 0 {
                        errors.push(format!("synthesized index rows: {}", work.automatic));
                    }
                }
                Err(error) => {
                    errors.push(format!("production statement work probe failed: {error}"))
                }
            }
            if !errors.is_empty() {
                failures.push(format!(
                    "{}: {errors:?}\nparameters={:?}\nresult={}\nraw plan={plan:#?}",
                    contract.operation, contract.parameters, contract.result
                ));
            }
        }
    }
    // Collect every contractual operator/work failure before this one assertion,
    // so a bundled SQLite update reports all named owners in a single run.
    assert!(
        failures.is_empty(),
        "{} query-plan owners failed:\n{}",
        failures.len(),
        failures.join("\n\n")
    );
    let reader = fixture.storage.begin_read().expect("reader policy");
    assert!(
        !reader
            .conn
            .db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_QPSG)
            .expect("reader QPSG policy")
    );
    drop(reader);
    for behavior in behaviors {
        behavior.verify(&fixture);
    }
    verify_snapshot_after_publication(&fixture);
}

#[test]
fn complete_plan_rejects_extra_work_and_an_unindexed_union_branch() {
    let case = Case::new(
        &read_syntax::FIND_QUALIFIED_SYMBOL,
        Vec::new(),
        Behavior::Symbols,
    );
    let node = |id, detail: &str| PlanNode {
        id,
        parent: 0,
        access: normalize_access(detail),
        detail: detail.into(),
    };
    let child = |id, parent, detail: &str| PlanNode {
        id,
        parent,
        access: normalize_access(detail),
        detail: detail.into(),
    };
    let base = vec![
        node(1, "MERGE (UNION ALL)"),
        child(2, 1, "LEFT"),
        child(3, 2, "SEARCH symbols USING INDEX symbols_name_idx (name=?)"),
        child(4, 1, "RIGHT"),
        child(
            5,
            4,
            "SEARCH symbols USING COVERING INDEX symbols_name_idx (name=?)",
        ),
    ];
    assert!(violations(&case, &base).is_empty());
    assert!(
        !violations(&case, &base[..4]).is_empty(),
        "both UNION branches must be protected"
    );
    for (position, id, parent, branch) in [(2, 3, 2, "LEFT"), (4, 5, 4, "RIGHT")] {
        let mut broad_range = base.clone();
        broad_range[position] = child(
            id,
            parent,
            "SEARCH symbols USING INDEX symbols_name_idx (name>?)",
        );
        assert_eq!(
            broad_range[position]
                .access
                .as_ref()
                .expect("range access")
                .indexed_terms,
            [IndexedTerm {
                column: "name".into(),
                comparison: IndexedComparison::Greater
            }]
        );
        let expected = format!("qualified-symbol {branch} branch requires indexed name equality");
        assert!(
            violations(&case, &broad_range)
                .iter()
                .any(|error| error.starts_with(&expected)),
            "a named-index range must not count as a name equality in the {branch} branch"
        );
    }
    let composite = normalize_access(
        "SEARCH chunks USING INDEX chunks_file_line_idx (file_id=? AND start_line<?) LEFT-JOIN",
    )
    .expect("composite indexed access");
    assert_eq!(
        composite.indexed_terms,
        [
            IndexedTerm {
                column: "file_id".into(),
                comparison: IndexedComparison::Equal
            },
            IndexedTerm {
                column: "start_line".into(),
                comparison: IndexedComparison::Less
            },
        ]
    );
    let mut same_branch = base.clone();
    same_branch[4] = child(5, 4, "SEARCH symbols USING INTEGER PRIMARY KEY (rowid=?)");
    same_branch.push(child(
        6,
        2,
        "SEARCH symbols USING INDEX symbols_name_idx (name=?)",
    ));
    assert_eq!(
        same_branch
            .iter()
            .filter_map(|node| node.access.as_ref())
            .filter(|access| access.index.as_deref() == Some("symbols_name_idx"))
            .count(),
        2
    );
    let errors = violations(&case, &same_branch);
    assert!(
        errors.iter().any(|error| error.contains("LEFT branch")),
        "two lookups in one branch escaped: {errors:?}"
    );
    assert!(
        errors.iter().any(|error| error.contains("RIGHT branch")),
        "a primary-key lookup replaced the other branch's name-index lookup: {errors:?}"
    );
    for detail in [
        "SCAN symbols",
        "SCAN files USING COVERING INDEX sqlite_autoindex_files_1",
        "SEARCH symbols USING AUTOMATIC COVERING INDEX (file_id=?)",
        "USE TEMP B-TREE FOR ORDER BY",
        "MATERIALIZE matches",
    ] {
        let mut regressed = base.clone();
        regressed.push(node(10, detail));
        assert!(
            !violations(&case, &regressed).is_empty(),
            "unapproved extra work escaped: {detail}"
        );
    }
    let mut tree = Case::new(&read_files::LIST_TREE_PATHS, Vec::new(), Behavior::Paths);
    tree.scans = &[("path_entries", IndexClass::Named)];
    let ordered = vec![node(
        1,
        "SCAN path_entries USING INDEX sqlite_autoindex_path_entries_1",
    )];
    assert!(violations(&tree, &ordered).is_empty());
    let mut sorted_ordered = ordered.clone();
    sorted_ordered.push(node(2, "USE TEMP B-TREE FOR ORDER BY"));
    assert!(
        !violations(&tree, &sorted_ordered).is_empty(),
        "ordered-path access must not acquire a sort"
    );
    let depth = vec![
        node(
            1,
            "SEARCH path_entries USING INDEX path_entries_depth_path_idx (depth<?)",
        ),
        node(2, "USE TEMP B-TREE FOR ORDER BY"),
    ];
    assert!(
        violations(&tree, &depth).is_empty(),
        "depth-index sort is a reviewed alternative"
    );
    let mut extra = depth;
    extra.push(node(
        3,
        "SCAN path_entries USING INDEX sqlite_autoindex_path_entries_1",
    ));
    assert!(
        !violations(&tree, &extra).is_empty(),
        "an additional path scan is not an alternative plan"
    );
}

// Review tripwire, not a Rust/SQL parser: adding a query branch, statement,
// or storage SQL owner requires an explicit disposition here. Existing ordinary
// CRUD remains behavior-owned; hot descriptors additionally require plan cases.
const SQL_DECISIONS: &[(&str, usize, usize, usize, &str)] = &[
    (
        "accounting.rs",
        0,
        2,
        0,
        "bounded metadata/helpers or caller-owned operations; existing behavior coverage",
    ),
    (
        "api.rs",
        0,
        0,
        0,
        "bounded metadata/helpers or caller-owned operations; existing behavior coverage",
    ),
    (
        "diagnostics.rs",
        1,
        0,
        0,
        "bounded metadata/helpers or caller-owned operations; existing behavior coverage",
    ),
    (
        "helpers.rs",
        5,
        0,
        0,
        "bounded metadata/helpers or caller-owned operations; existing behavior coverage",
    ),
    (
        "mapping.rs",
        0,
        0,
        0,
        "bounded metadata/helpers or caller-owned operations; existing behavior coverage",
    ),
    (
        "mod.rs",
        0,
        0,
        0,
        "bounded metadata/helpers or caller-owned operations; existing behavior coverage",
    ),
    (
        "models.rs",
        0,
        0,
        0,
        "bounded metadata/helpers or caller-owned operations; existing behavior coverage",
    ),
    (
        "open.rs",
        17,
        7,
        0,
        "connection/bootstrap/lifecycle probes; schema migrations have a separate owner",
    ),
    (
        "projections.rs",
        55,
        6,
        5,
        "publication/generation and projection repair; hot statements inventoried separately",
    ),
    (
        "publication.rs",
        4,
        2,
        2,
        "publication/generation and projection repair; hot statements inventoried separately",
    ),
    (
        "query_receipts.rs",
        8,
        3,
        2,
        "quota-bounded persistence; hot lookup/order/pruning statements inventoried separately",
    ),
    (
        "read/counts.rs",
        8,
        0,
        0,
        "retrieval statements retain existing API limits; selected hot paths have plan contracts",
    ),
    (
        "read/files.rs",
        5,
        0,
        0,
        "retrieval statements retain existing API limits; selected hot paths have plan contracts",
    ),
    (
        "read/imports.rs",
        7,
        0,
        0,
        "retrieval statements retain existing API limits; selected hot paths have plan contracts",
    ),
    (
        "read/meta.rs",
        7,
        0,
        0,
        "retrieval statements retain existing API limits; selected hot paths have plan contracts",
    ),
    (
        "read/search.rs",
        23,
        0,
        0,
        "retrieval statements retain existing API limits; selected hot paths have plan contracts",
    ),
    (
        "read/syntax.rs",
        22,
        0,
        0,
        "retrieval statements retain existing API limits; selected hot paths have plan contracts",
    ),
    (
        "read_delta.rs",
        6,
        3,
        4,
        "quota-bounded persistence; hot lookup/order/pruning statements inventoried separately",
    ),
    (
        "receipts.rs",
        10,
        3,
        4,
        "quota-bounded persistence; hot lookup/order/pruning statements inventoried separately",
    ),
    (
        "runtime.rs",
        0,
        0,
        0,
        "bounded metadata/helpers or caller-owned operations; existing behavior coverage",
    ),
    (
        "scoped_regex.rs",
        0,
        0,
        0,
        "bounded metadata/helpers or caller-owned operations; existing behavior coverage",
    ),
    (
        "session.rs",
        0,
        0,
        0,
        "bounded metadata/helpers or caller-owned operations; existing behavior coverage",
    ),
    (
        "snapshot.rs",
        0,
        0,
        0,
        "bounded metadata/helpers or caller-owned operations; existing behavior coverage",
    ),
    (
        "staging.rs",
        10,
        1,
        0,
        "publication/generation and projection repair; hot statements inventoried separately",
    ),
    (
        "writer.rs",
        1,
        2,
        3,
        "publication/generation and projection repair; hot statements inventoried separately",
    ),
];

#[test]
fn plan_inventory_has_behavior_and_reviewed_sql_owners() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/storage");
    let inventory = HOT_QUERIES
        .iter()
        .map(|query| query.contract.operation)
        .collect::<HashSet<_>>();
    assert_eq!(
        inventory.len(),
        HOT_QUERIES.len(),
        "duplicate hot operation names"
    );
    let mut declarations = HashSet::new();
    let mut found_paths = HashSet::new();
    let mut directories = vec![root.clone()];
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(&directory).expect("storage source inventory") {
            let path = entry.expect("source entry").path();
            if path.is_dir() {
                if path.file_name().and_then(|name| name.to_str()) != Some("tests") {
                    directories.push(path);
                }
                continue;
            }
            let relative = path
                .strip_prefix(&root)
                .expect("storage path")
                .to_string_lossy()
                .replace('\\', "/");
            if matches!(
                relative.as_str(),
                "schema.rs" | "tests.rs" | "query_plans.rs" | "hot_query.rs"
            ) {
                continue;
            }
            if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
                continue;
            }
            found_paths.insert(relative.clone());
            let decision = SQL_DECISIONS
                .iter()
                .find(|(name, ..)| *name == relative)
                .unwrap_or_else(|| {
                    panic!(
                        "new storage SQL owner needs an explicit query-plan decision: {relative}"
                    )
                });
            let source = fs::read_to_string(&path).expect("owner source");
            assert_eq!(
                (
                    source.matches("SELECT").count(),
                    source.matches("UPDATE ").count(),
                    source.matches("DELETE FROM ").count()
                ),
                (decision.1, decision.2, decision.3),
                "{relative}: SQL surface changed; review bounds and add a hot descriptor or update the documented decision: {}",
                decision.4
            );
            assert!(!decision.4.is_empty());
            assert!(
                !source.contains("ANALYZE") && !source.contains("PRAGMA optimize"),
                "{relative}: review the no-statistics planner policy"
            );
            let compact_source = source
                .chars()
                .filter(|character| !character.is_whitespace())
                .collect::<String>();
            for declaration in source.split("hot_query!(").skip(1) {
                let name = declaration
                    .split(',')
                    .next()
                    .expect("descriptor name")
                    .trim();
                let operation = declaration.split('"').nth(1).expect("semantic operation");
                assert!(
                    declarations.insert(operation.to_owned()),
                    "duplicate descriptor {operation}"
                );
                assert!(
                    compact_source.contains(&format!("{name}.sql")),
                    "{name}: production must consume the descriptor"
                );
            }
        }
    }
    assert_eq!(
        found_paths,
        SQL_DECISIONS
            .iter()
            .map(|(path, ..)| (*path).to_owned())
            .collect()
    );
    assert_eq!(
        declarations,
        inventory.iter().map(|name| (*name).to_owned()).collect()
    );
    for (owner, source) in [
        ("storage behavior", include_str!("tests.rs")),
        ("receipt behavior", include_str!("tests/receipts.rs")),
        (
            "query-receipt behavior",
            include_str!("tests/query_receipts.rs"),
        ),
        ("read-delta behavior", include_str!("tests/read_delta.rs")),
        (
            "storage domain",
            include_str!("../../crates/test-suite/src/domains/storage.rs"),
        ),
    ] {
        assert!(
            !source.contains("EXPLAIN QUERY PLAN"),
            "{owner}: plan diagnostics belong to storage::query_plans and production SQL descriptors"
        );
    }
}

fn verify_snapshot_after_publication(fixture: &Fixture) {
    let old = fixture
        .storage
        .begin_read()
        .expect("pinned hot-query snapshot");
    let generation = old.repository_generation().expect("pin generation");
    let partition = old
        .exact_query_partition(|_| true, || Ok(()))
        .expect("old partition");
    fixture
        .storage
        .reconcile_files(
            "config",
            vec![
                sample_file("src/target.rs", "changed\n"),
                sample_file("src/new.rs", "new\n"),
            ],
            &["src/importer-a.rs".into()],
        )
        .expect("publish across pinned queries");
    let new = fixture.storage.begin_read().expect("new snapshot");
    let new_target = new
        .find_file("src/target.rs")
        .expect("published target")
        .expect("target exists")
        .id;
    assert_eq!(
        old.repository_generation().expect("old generation"),
        generation
    );
    assert_eq!(
        new.repository_generation().expect("new generation"),
        generation + 1
    );
    assert_eq!(
        old.affected_importers(&["src/target.rs".into()])
            .expect("old imports")
            .len(),
        2
    );
    assert_eq!(
        new.affected_importers(&["src/target.rs".into()])
            .expect("new imports")
            .len(),
        1
    );
    assert_eq!(
        old.get_chunks_overlapping_batch(&[(fixture.target_id, 1, 8)])
            .expect("old ranges")[0]
            .len(),
        4
    );
    assert_eq!(
        new.get_chunks_overlapping_batch(&[(new_target, 1, 8)])
            .expect("new ranges")[0]
            .len(),
        1
    );
    assert!(matches!(
        old.find_symbol(fixture.target_id, "unique")
            .expect("old exact symbol"),
        crate::symbol_identity::SymbolResolution::Unique(_)
    ));
    assert!(matches!(
        new.find_symbol(new_target, "unique")
            .expect("new exact symbol"),
        crate::symbol_identity::SymbolResolution::NotFound
    ));
    assert_eq!(
        old.search_symbols("Services.selective", true, PAGE)
            .expect("old FTS projection")
            .len(),
        2
    );
    assert!(
        new.search_symbols("Services.selective", true, PAGE)
            .expect("new FTS projection")
            .is_empty()
    );
    assert!(
        !old.list_tree_paths("src", 1, None, HARD_MAX_RESULTS)
            .expect("old path page")
            .iter()
            .any(|entry| entry.path == "src/new.rs")
    );
    assert!(
        new.list_glob_paths("src/new*", None, None, PAGE)
            .expect("new path page")
            .iter()
            .any(|entry| entry.path == "src/new.rs")
    );
    assert_eq!(
        old.exact_query_partition(|_| true, || Ok(()))
            .expect("pinned partition")
            .digest,
        partition.digest
    );
    assert_ne!(
        new.exact_query_partition(|_| true, || Ok(()))
            .expect("published partition")
            .digest,
        partition.digest
    );
}

#[test]
fn tree_plan_alternatives_preserve_results_and_fixture_work_bounds() {
    // Neither index dominates: flat early pages favor path order, while shallow
    // pages in a deep repository favor depth filtering. Keep production SQL
    // unchanged and compare both reviewed alternatives against the storage API.
    for size in [64, 256] {
        for deep in [false, true] {
            let directory = tempfile::tempdir().expect("tree comparison directory");
            let storage =
                Storage::open(directory.path().join("index.sqlite")).expect("tree storage");
            let mut paths = vec![
                "src/target.rs".to_owned(),
                "src/importer-a.rs".to_owned(),
                "src/importer-b.rs".to_owned(),
            ];
            paths.extend((0..size).map(|index| {
                if deep {
                    format!("src/d{:02}/nested/noise-{index:04}.rs", index % 8)
                } else {
                    format!("src/noise-{index:04}.rs")
                }
            }));
            storage
                .full_reconcile(
                    "config",
                    paths.iter().map(|path| sample_file(path, "x\n")).collect(),
                )
                .expect("publish tree comparison");
            let session = storage.begin_read().expect("tree comparison snapshot");
            let projected: i64 = session
                .conn
                .query_row("SELECT count(*) FROM path_entries", [], |row| row.get(0))
                .expect("projection cardinality");
            let projected = usize::try_from(projected).expect("nonnegative projection size");
            let cursor = if deep {
                "src/d03".to_owned()
            } else {
                format!("src/noise-{:04}.rs", size / 2)
            };
            for (request, root, depth, after) in [
                ("broad_first", "src", 1, None),
                ("late_page", "src", 1, Some(cursor.as_str())),
                ("root_only", "src", 0, None),
                ("absent_root", "absent", 1, None),
            ] {
                let expected = session
                    .list_tree_paths(root, depth, after, PAGE)
                    .expect("real tree API")
                    .into_iter()
                    .map(|record| record.path)
                    .collect::<Vec<_>>();
                assert!(expected.len() <= PAGE);
                let parameters = vec![
                    text(root),
                    integer(i64::try_from(depth + 1).expect("depth")),
                    after.map_or(Value::Null, text),
                    integer(i64::try_from(PAGE).expect("page")),
                ];
                for (variant, sql) in [
                    ("production", read_files::LIST_TREE_PATHS.sql.to_owned()),
                    (
                        "ordered_path",
                        read_files::LIST_TREE_PATHS.sql.replace(
                            "FROM path_entries",
                            "FROM path_entries INDEXED BY sqlite_autoindex_path_entries_1",
                        ),
                    ),
                    (
                        "depth_order",
                        read_files::LIST_TREE_PATHS.sql.replace(
                            "FROM path_entries",
                            "FROM path_entries INDEXED BY path_entries_depth_path_idx",
                        ),
                    ),
                ] {
                    let mut case = Case::new(
                        &read_files::LIST_TREE_PATHS,
                        parameters.clone(),
                        Behavior::Paths,
                    );
                    case.scans = &[("path_entries", IndexClass::Named)];
                    let plan =
                        explain(&session.conn, &sql, &parameters).expect("tree alternative plan");
                    let errors = violations(&case, &plan);
                    assert!(
                        errors.is_empty(),
                        "size={size}, deep={deep}, request={request}, variant={variant}: {errors:?}; raw plan={plan:#?}"
                    );
                    let mut statement = session
                        .conn
                        .prepare(&sql)
                        .expect("tree alternative statement");
                    let actual = statement
                        .query_map(rusqlite::params_from_iter(&parameters), |row| {
                            row.get::<_, String>(0)
                        })
                        .expect("tree alternative rows")
                        .collect::<rusqlite::Result<Vec<_>>>()
                        .expect("complete tree alternative");
                    assert_eq!(
                        actual, expected,
                        "size={size}, deep={deep}, request={request}, variant={variant}"
                    );
                    let vm = usize::try_from(statement.get_status(StatementStatus::VmStep))
                        .expect("nonnegative VM count");
                    let fullscan =
                        usize::try_from(statement.get_status(StatementStatus::FullscanStep))
                            .expect("nonnegative fullscan count");
                    let sorts = statement.get_status(StatementStatus::Sort);
                    assert_eq!(statement.get_status(StatementStatus::AutoIndex), 0);
                    // Explicit cardinality-dependent diagnostic bounds. LIMIT
                    // bounds returned rows, not the number of entries visited.
                    assert!(
                        vm <= projected * 80 + 256,
                        "excessive tree work: {vm} VM steps for {projected} projected rows"
                    );
                    assert!(
                        fullscan <= projected,
                        "tree scanned more than its fixture projection"
                    );
                    assert!(sorts <= 1);
                    eprintln!(
                        "tree comparison SQLite={}: files={}, projected={projected}, deep={deep}, request={request}, variant={variant}, rows={}, vm={vm}, fullscan={fullscan}, sorts={sorts}; raw plan={plan:#?}",
                        rusqlite::version(),
                        size + 3,
                        actual.len()
                    );
                }
            }
        }
    }
}
