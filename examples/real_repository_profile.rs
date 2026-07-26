//! Profile retrieval behavior against an existing repository without modifying it.
//!
//! The source repository is opened read-only by convention. Its index is stored
//! in an anonymous temporary directory and removed when the process exits.
//!
//! ```bash
//! cargo run --example real_repository_profile --release -- \
//!   --repository /root/openclaw --iterations 3
//! ```

use std::collections::{BTreeMap, HashMap, HashSet};
use std::error::Error as StdError;
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;
use leantoken::indexer::Indexer;
use leantoken::model::{
    ContextRequest, ReadRequest, RetrievalPrimitiveKey, SearchEvaluation, SearchMode, SearchRequest,
};
use leantoken::services::Services;
use leantoken::storage::Storage;
use leantoken::{Config, Result as LeanTokenResult};
use serde::Serialize;
use serde_json::{Value, json};

type AnyResult<T> = Result<T, Box<dyn StdError>>;

#[derive(Debug, Parser)]
#[command(about = "Profile LeanToken retrieval against a real repository")]
struct Args {
    /// Existing repository to index into a disposable database.
    #[arg(long)]
    repository: PathBuf,
    /// Number of warmed retrieval samples per shape.
    #[arg(long, default_value_t = 3)]
    iterations: usize,
    /// Persistent index path. Omit to remove the disposable index after the run.
    #[arg(long)]
    database: Option<PathBuf>,
    /// Reuse an existing caller-provided index without reconciling the repository.
    #[arg(long, requires = "database")]
    skip_index: bool,
    /// Recreate the four empty FTS5 indexes with columnsize=0 before indexing.
    #[arg(long, conflicts_with = "skip_index")]
    fts_columnsize_zero: bool,
    /// Indexed source used for shallow, deep, complete, and truncated reads.
    #[arg(long, default_value = "src/cli/update-cli.test.ts")]
    read_path: String,
}

#[derive(Debug, Serialize)]
struct TimingStats {
    samples: usize,
    p50_ms: f64,
    p95_ms: f64,
    mean_ms: f64,
}

#[tokio::main]
async fn main() -> AnyResult<()> {
    let args = Args::parse();
    if args.iterations == 0 {
        return Err("iterations must be positive".into());
    }
    let root = args.repository.canonicalize()?;
    let read_source = fs::read_to_string(root.join(&args.read_path))?;
    let read_lines = read_source.lines().count().max(1);
    let repository = repository_metadata(&root)?;

    let cache = args
        .database
        .is_none()
        .then(tempfile::tempdir)
        .transpose()?;
    let database = args.database.clone().unwrap_or_else(|| {
        cache
            .as_ref()
            .expect("missing database creates a temporary cache")
            .path()
            .join("repository.sqlite")
    });
    let config = Config::discover(&root, Some(database.clone()))?;
    if args.fts_columnsize_zero {
        initialize_columnsize_zero_schema(&config)?;
    }
    let index = if args.skip_index {
        json!({
            "skipped": true,
            "database_bytes": fs::metadata(&database)?.len(),
        })
    } else {
        let storage = Storage::open(&config.database_path)?;
        let indexer = Indexer::new(Arc::new(config.clone()), storage)?;
        let index_started = Instant::now();
        let profile = indexer.reconcile_profiled_report(false)?;
        let response = &profile.report.response;
        json!({
            "skipped": false,
            "generation": response.repository_generation,
            "files_seen": response.files_seen,
            "files_indexed": response.files_indexed,
            "files_skipped": response.files_skipped,
            "warnings": response.warnings,
            "elapsed_ms": milliseconds(index_started.elapsed()),
            "database_bytes": fs::metadata(&database)?.len(),
            "diagnostics": profile.diagnostics,
            "fts_columnsize_zero": args.fts_columnsize_zero,
        })
    };
    let fts_columnsize = detect_fts_columnsize(&database)?;
    let services = Services::open(config)?;

    let regex = profile_regex_matrix(&services, args.iterations).await;
    let regex_selectivity = profile_regex_selectivity(&database, args.iterations.max(3))?;
    let lexical_ranking = profile_lexical_ranking(&database, args.iterations.max(3))?;
    let (context, reuse) = profile_context_matrix(&services, args.iterations).await?;
    let reads = profile_reads(
        &services,
        &args.read_path,
        read_lines,
        read_source.len(),
        args.iterations,
    )
    .await?;

    let report = json!({
        "schema_version": 1,
        "release_build": !cfg!(debug_assertions),
        "repository": repository,
        "index": index,
        "regex": regex,
        "regex_selectivity": regex_selectivity,
        "lexical_ranking": lexical_ranking,
        "context": context,
        "reads": reads,
        "repeated_primitive_trace": reuse,
        "methodology": {
            "source_tree_modified": false,
            "database_lifetime": if args.database.is_some() {
                "caller-provided persistent path"
            } else {
                "anonymous temporary directory"
            },
            "timings": "warm process and operating-system cache; no timing assertions",
            "regex_reference": "candidate planning enabled versus bounded full scan",
            "regex_selectivity": "current full mandatory-literal MATCH versus a Zoekt-style rarest mandatory trigram pair; frequency-probe cost reported separately",
            "lexical_ranking": "current bm25 ordering versus SQLite rank ordering and bounded rank-first hydration; result identity is reported, never assumed",
            "primitive_keys": "generation-scoped BLAKE3 identities; raw inputs omitted",
            "fts_columnsize": fts_columnsize,
        },
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn detect_fts_columnsize(database: &Path) -> AnyResult<u8> {
    let connection = rusqlite::Connection::open_with_flags(
        database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let docsize_tables = connection.query_row(
        "SELECT count(*)
         FROM sqlite_master
         WHERE type = 'table'
           AND name IN (
             'chunks_fts_word_docsize',
             'chunks_fts_trigram_docsize',
             'symbols_fts_trigram_docsize',
             'symbol_refs_fts_trigram_docsize'
           )",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    match docsize_tables {
        0 => Ok(0),
        4 => Ok(1),
        count => {
            Err(format!("mixed FTS columnsize schema: found {count} of four docsize tables").into())
        }
    }
}

fn repository_metadata(root: &Path) -> AnyResult<Value> {
    let head = git(root, &["rev-parse", "HEAD"])?;
    let status = git(root, &["status", "--porcelain"])?;
    Ok(json!({
        "root": root,
        "git_head": head.trim(),
        "git_clean": status.trim().is_empty(),
    }))
}

fn git(root: &Path, args: &[&str]) -> AnyResult<String> {
    let output = Command::new("git").args(args).current_dir(root).output()?;
    if !output.status.success() {
        return Err(format!("git {} failed", args.join(" ")).into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn initialize_columnsize_zero_schema(config: &Config) -> AnyResult<()> {
    drop(Storage::open(&config.database_path)?);
    let mut connection = rusqlite::Connection::open(&config.database_path)?;
    let transaction = connection.transaction()?;
    let generation = transaction.query_row(
        "SELECT repository_generation FROM meta WHERE id = 1",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if generation != 0 {
        return Err("--fts-columnsize-zero requires an empty index".into());
    }
    transaction.execute_batch(
        r#"
        DROP TRIGGER chunks_ai_word;
        DROP TRIGGER chunks_ad_word;
        DROP TRIGGER chunks_au_word;
        DROP TRIGGER chunks_ai_trigram;
        DROP TRIGGER chunks_ad_trigram;
        DROP TRIGGER chunks_au_trigram;
        DROP TRIGGER symbols_ai_trigram;
        DROP TRIGGER symbols_ad_trigram;
        DROP TRIGGER symbols_au_trigram;
        DROP TRIGGER symbol_refs_ai_trigram;
        DROP TRIGGER symbol_refs_ad_trigram;
        DROP TRIGGER symbol_refs_au_trigram;
        DROP TABLE chunks_fts_word;
        DROP TABLE chunks_fts_trigram;
        DROP TABLE symbols_fts_trigram;
        DROP TABLE symbol_refs_fts_trigram;

        CREATE VIRTUAL TABLE chunks_fts_word USING fts5(
            content,
            content='chunks',
            content_rowid='rowid',
            tokenize='unicode61',
            columnsize=0
        );
        CREATE VIRTUAL TABLE chunks_fts_trigram USING fts5(
            content,
            content='chunks',
            content_rowid='rowid',
            tokenize='trigram',
            columnsize=0
        );
        CREATE VIRTUAL TABLE symbols_fts_trigram USING fts5(
            name,
            content='symbols',
            content_rowid='rowid',
            tokenize='trigram',
            columnsize=0
        );
        CREATE VIRTUAL TABLE symbol_refs_fts_trigram USING fts5(
            name,
            content='symbol_refs',
            content_rowid='rowid',
            tokenize='trigram',
            columnsize=0
        );

        CREATE TRIGGER chunks_ai_word AFTER INSERT ON chunks BEGIN
            INSERT INTO chunks_fts_word(rowid, content) VALUES (new.rowid, new.content);
        END;
        CREATE TRIGGER chunks_ad_word AFTER DELETE ON chunks BEGIN
            INSERT INTO chunks_fts_word(chunks_fts_word, rowid, content)
            VALUES ('delete', old.rowid, old.content);
        END;
        CREATE TRIGGER chunks_au_word AFTER UPDATE ON chunks BEGIN
            INSERT INTO chunks_fts_word(chunks_fts_word, rowid, content)
            VALUES ('delete', old.rowid, old.content);
            INSERT INTO chunks_fts_word(rowid, content) VALUES (new.rowid, new.content);
        END;
        CREATE TRIGGER chunks_ai_trigram AFTER INSERT ON chunks BEGIN
            INSERT INTO chunks_fts_trigram(rowid, content) VALUES (new.rowid, new.content);
        END;
        CREATE TRIGGER chunks_ad_trigram AFTER DELETE ON chunks BEGIN
            INSERT INTO chunks_fts_trigram(chunks_fts_trigram, rowid, content)
            VALUES ('delete', old.rowid, old.content);
        END;
        CREATE TRIGGER chunks_au_trigram AFTER UPDATE ON chunks BEGIN
            INSERT INTO chunks_fts_trigram(chunks_fts_trigram, rowid, content)
            VALUES ('delete', old.rowid, old.content);
            INSERT INTO chunks_fts_trigram(rowid, content) VALUES (new.rowid, new.content);
        END;
        CREATE TRIGGER symbols_ai_trigram AFTER INSERT ON symbols BEGIN
            INSERT INTO symbols_fts_trigram(rowid, name) VALUES (new.rowid, new.name);
        END;
        CREATE TRIGGER symbols_ad_trigram AFTER DELETE ON symbols BEGIN
            INSERT INTO symbols_fts_trigram(symbols_fts_trigram, rowid, name)
            VALUES ('delete', old.rowid, old.name);
        END;
        CREATE TRIGGER symbols_au_trigram AFTER UPDATE ON symbols BEGIN
            INSERT INTO symbols_fts_trigram(symbols_fts_trigram, rowid, name)
            VALUES ('delete', old.rowid, old.name);
            INSERT INTO symbols_fts_trigram(rowid, name) VALUES (new.rowid, new.name);
        END;
        CREATE TRIGGER symbol_refs_ai_trigram AFTER INSERT ON symbol_refs BEGIN
            INSERT INTO symbol_refs_fts_trigram(rowid, name) VALUES (new.rowid, new.name);
        END;
        CREATE TRIGGER symbol_refs_ad_trigram AFTER DELETE ON symbol_refs BEGIN
            INSERT INTO symbol_refs_fts_trigram(symbol_refs_fts_trigram, rowid, name)
            VALUES ('delete', old.rowid, old.name);
        END;
        CREATE TRIGGER symbol_refs_au_trigram AFTER UPDATE ON symbol_refs BEGIN
            INSERT INTO symbol_refs_fts_trigram(symbol_refs_fts_trigram, rowid, name)
            VALUES ('delete', old.rowid, old.name);
            INSERT INTO symbol_refs_fts_trigram(rowid, name) VALUES (new.rowid, new.name);
        END;
        "#,
    )?;
    transaction.commit()?;
    Ok(())
}

async fn profile_regex_matrix(services: &Services, iterations: usize) -> Value {
    let shapes = [
        (
            "negative_planned",
            r"leantoken_openclaw_absent_92741\s*=\s*never",
            true,
        ),
        ("sparse_positive_planned", r"startGatewayServer\s*\(", true),
        ("common_positive_planned", r"describe\s*\(", true),
        (
            "sparse_positive_case_insensitive_fallback",
            r"startgatewayserver\s*\(",
            false,
        ),
    ];
    let mut report = serde_json::Map::new();
    for (label, query, case_sensitive) in shapes {
        let request = regex_request(query, case_sensitive);
        report.insert(
            label.into(),
            profile_regex_shape(services, request, iterations).await,
        );
    }
    Value::Object(report)
}

fn profile_regex_selectivity(database: &Path, iterations: usize) -> AnyResult<Value> {
    const COUNT_CAP: usize = 10_001;
    let connection = rusqlite::Connection::open_with_flags(
        database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    connection.execute_batch(
        "CREATE VIRTUAL TABLE temp.profile_trigram_vocab
         USING fts5vocab(main, chunks_fts_trigram, row)",
    )?;
    let shapes = [
        ("negative", vec!["leantoken_openclaw_absent_92741", "never"]),
        ("sparse_positive", vec!["startGatewayServer"]),
        ("common_positive", vec!["describe"]),
        ("compound_positive", vec!["gateway", "authentication"]),
    ];
    let mut report = serde_json::Map::new();
    for (label, literals) in shapes {
        let full_query = literals
            .iter()
            .map(|literal| quote_fts(literal))
            .collect::<Vec<_>>()
            .join(" AND ");
        let full = measure_fts_candidate_count(&connection, &full_query, COUNT_CAP, iterations)?;

        let probe_started = Instant::now();
        let trigrams = mandatory_ascii_trigrams(&literals);
        let mut trigram_frequencies = trigram_document_frequencies(&connection, &trigrams)?;
        let frequency_probe_ms = milliseconds(probe_started.elapsed());
        trigram_frequencies
            .sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.0.cmp(&right.0)));
        let rarest = trigram_frequencies
            .iter()
            .take(2)
            .cloned()
            .collect::<Vec<_>>();
        let pair_query = rarest
            .iter()
            .map(|(trigram, _)| quote_fts(trigram))
            .collect::<Vec<_>>()
            .join(" AND ");
        let pair = measure_fts_candidate_count(&connection, &pair_query, COUNT_CAP, iterations)?;
        report.insert(
            label.into(),
            json!({
                "mandatory_literals": literals,
                "current_full_literal": {
                    "query": full_query,
                    "candidate_count_capped": full.0,
                    "timing_ms": full.1,
                },
                "rarest_trigram_pair": {
                    "query": pair_query,
                    "trigrams_and_document_frequencies": rarest,
                    "candidate_count_capped": pair.0,
                    "timing_ms": pair.1,
                    "frequency_probe_ms": frequency_probe_ms,
                    "trigrams_probed": trigram_frequencies.len(),
                },
            }),
        );
    }
    Ok(Value::Object(report))
}

fn profile_lexical_ranking(database: &Path, iterations: usize) -> AnyResult<Value> {
    const LIMIT: usize = 128;
    const CURRENT: &str = "
        SELECT c.id, c.content, bm25(chunks_fts_trigram)
        FROM chunks_fts_trigram
        JOIN chunks c ON chunks_fts_trigram.rowid = c.rowid
        JOIN files f ON c.file_id = f.id
        WHERE chunks_fts_trigram MATCH ?1
        ORDER BY bm25(chunks_fts_trigram), f.path, c.start_byte
        LIMIT ?2";
    const RANK_WITH_TIEBREAK: &str = "
        SELECT c.id, c.content, chunks_fts_trigram.rank
        FROM chunks_fts_trigram
        JOIN chunks c ON chunks_fts_trigram.rowid = c.rowid
        JOIN files f ON c.file_id = f.id
        WHERE chunks_fts_trigram MATCH ?1
        ORDER BY chunks_fts_trigram.rank, f.path, c.start_byte
        LIMIT ?2";
    const RANK_ONLY: &str = "
        SELECT c.id, c.content, chunks_fts_trigram.rank
        FROM chunks_fts_trigram
        JOIN chunks c ON chunks_fts_trigram.rowid = c.rowid
        WHERE chunks_fts_trigram MATCH ?1
        ORDER BY chunks_fts_trigram.rank
        LIMIT ?2";
    const RANK_FIRST: &str = "
        WITH ranked AS MATERIALIZED (
            SELECT rowid, rank AS score
            FROM chunks_fts_trigram
            WHERE chunks_fts_trigram MATCH ?1
            ORDER BY rank
            LIMIT ?2
        )
        SELECT c.id, c.content, ranked.score
        FROM ranked
        JOIN chunks c ON ranked.rowid = c.rowid
        JOIN files f ON c.file_id = f.id
        ORDER BY ranked.score, f.path, c.start_byte";
    let connection = rusqlite::Connection::open_with_flags(
        database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let variants = [
        ("current_bm25_with_tiebreak", CURRENT),
        ("rank_with_tiebreak", RANK_WITH_TIEBREAK),
        ("rank_only", RANK_ONLY),
        ("rank_first_hydration", RANK_FIRST),
    ];
    let mut shapes = serde_json::Map::new();
    for (label, term) in [
        ("gateway", "gateway"),
        ("authentication", "authentication"),
        ("configuration", "configuration"),
        ("startup", "startup"),
    ] {
        let query = quote_fts(term);
        let baseline = lexical_query_rows(&connection, CURRENT, &query, LIMIT)?;
        let baseline_ids = baseline.iter().map(|row| row.id).collect::<Vec<_>>();
        let baseline_scores = baseline
            .iter()
            .map(|row| row.score.to_bits())
            .collect::<Vec<_>>();
        let baseline_id_set = baseline_ids.iter().copied().collect::<HashSet<_>>();
        let mut measurements = serde_json::Map::new();
        for (variant, sql) in variants {
            let rows = lexical_query_rows(&connection, sql, &query, LIMIT)?;
            let ids = rows.iter().map(|row| row.id).collect::<Vec<_>>();
            let scores = rows
                .iter()
                .map(|row| row.score.to_bits())
                .collect::<Vec<_>>();
            let id_set = ids.iter().copied().collect::<HashSet<_>>();
            let mut durations = Vec::with_capacity(iterations);
            for _ in 0..iterations {
                let started = Instant::now();
                let measured = lexical_query_rows(&connection, sql, &query, LIMIT)?;
                black_box(&measured);
                durations.push(started.elapsed());
            }
            measurements.insert(
                variant.into(),
                json!({
                    "rows": rows.len(),
                    "same_order_as_current": ids == baseline_ids,
                    "same_set_as_current": id_set == baseline_id_set,
                    "same_scores_as_current": scores == baseline_scores,
                    "timing_ms": TimingStats::from_durations(durations),
                    "query_plan": explain_query_plan(&connection, sql, &query, LIMIT)?,
                }),
            );
        }
        shapes.insert(label.into(), Value::Object(measurements));
    }
    Ok(Value::Object(shapes))
}

struct ProfiledLexicalRow {
    id: i64,
    #[allow(dead_code)]
    content: String,
    score: f64,
}

fn lexical_query_rows(
    connection: &rusqlite::Connection,
    sql: &str,
    query: &str,
    limit: usize,
) -> AnyResult<Vec<ProfiledLexicalRow>> {
    let limit = i64::try_from(limit)?;
    let mut statement = connection.prepare_cached(sql)?;
    let rows = statement.query_map(rusqlite::params![query, limit], |row| {
        Ok(ProfiledLexicalRow {
            id: row.get(0)?,
            content: row.get(1)?,
            score: row.get(2)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn explain_query_plan(
    connection: &rusqlite::Connection,
    sql: &str,
    query: &str,
    limit: usize,
) -> AnyResult<Vec<String>> {
    let explain = format!("EXPLAIN QUERY PLAN {sql}");
    let limit = i64::try_from(limit)?;
    let mut statement = connection.prepare(&explain)?;
    let rows = statement.query_map(rusqlite::params![query, limit], |row| row.get(3))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn trigram_document_frequencies(
    connection: &rusqlite::Connection,
    trigrams: &[String],
) -> AnyResult<Vec<(String, usize)>> {
    let input = serde_json::to_string(trigrams)?;
    let mut statement = connection.prepare(
        "WITH requested AS (
             SELECT CAST(key AS INTEGER) AS request_index,
                    CAST(value AS TEXT) AS term
             FROM json_each(?1)
         )
         SELECT requested.term, COALESCE(vocabulary.doc, 0)
         FROM requested
         LEFT JOIN temp.profile_trigram_vocab AS vocabulary
           ON vocabulary.term = requested.term
         ORDER BY requested.request_index",
    )?;
    let rows = statement.query_map([input], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    rows.map(|row| {
        let (trigram, count) = row?;
        Ok((trigram, usize::try_from(count)?))
    })
    .collect()
}

fn mandatory_ascii_trigrams(literals: &[&str]) -> Vec<String> {
    let mut unique = HashSet::new();
    for literal in literals {
        if !literal.is_ascii() {
            continue;
        }
        let folded = literal.to_ascii_lowercase();
        for bytes in folded.as_bytes().windows(3) {
            unique.insert(String::from_utf8_lossy(bytes).into_owned());
        }
    }
    let mut trigrams = unique.into_iter().collect::<Vec<_>>();
    trigrams.sort();
    trigrams
}

fn quote_fts(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn capped_fts_candidate_count(
    connection: &rusqlite::Connection,
    query: &str,
    cap: usize,
) -> AnyResult<usize> {
    let cap = i64::try_from(cap)?;
    let count = connection.query_row(
        "SELECT count(*) FROM (
             SELECT rowid
             FROM chunks_fts_trigram
             WHERE chunks_fts_trigram MATCH ?1
             LIMIT ?2
         )",
        rusqlite::params![query, cap],
        |row| row.get::<_, i64>(0),
    )?;
    Ok(usize::try_from(count)?)
}

fn measure_fts_candidate_count(
    connection: &rusqlite::Connection,
    query: &str,
    cap: usize,
    iterations: usize,
) -> AnyResult<(usize, TimingStats)> {
    let count = capped_fts_candidate_count(connection, query, cap)?;
    let mut durations = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let started = Instant::now();
        black_box(capped_fts_candidate_count(connection, query, cap)?);
        durations.push(started.elapsed());
    }
    Ok((count, TimingStats::from_durations(durations)))
}

async fn profile_regex_shape(
    services: &Services,
    request: SearchRequest,
    iterations: usize,
) -> Value {
    let optimized = services.search_evaluation(request.clone()).await;
    let full_scan = services.search_full_scan_evaluation(request.clone()).await;
    let optimized_outcome = search_outcome(&optimized);
    let full_scan_outcome = search_outcome(&full_scan);
    let parity = match (&optimized, &full_scan) {
        (Ok(left), Ok(right)) => {
            serde_json::to_value(&left.response).ok() == serde_json::to_value(&right.response).ok()
        }
        (Err(left), Err(right)) => left.to_string() == right.to_string(),
        _ => false,
    };

    let mut durations = Vec::with_capacity(iterations);
    let mut repeated_outcomes = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let started = Instant::now();
        let current = services.search_evaluation(request.clone()).await;
        durations.push(started.elapsed());
        repeated_outcomes.push(search_outcome(&current));
        black_box(&current);
    }
    json!({
        "query": request.query,
        "case_sensitive": request.case_sensitive,
        "optimized": optimized_outcome,
        "full_scan": full_scan_outcome,
        "differential_parity": parity,
        "optimized_phases": optimized.as_ref().ok().map(|evaluation| &evaluation.phases),
        "timing_ms": TimingStats::from_durations(durations),
        "repeated_outcomes": repeated_outcomes,
    })
}

fn search_outcome(result: &LeanTokenResult<SearchEvaluation>) -> Value {
    match result {
        Ok(evaluation) => json!({
            "status": "ok",
            "hits": evaluation.response.hits.len(),
            "generation": evaluation.response.meta.repository_generation,
        }),
        Err(error) => json!({
            "status": "error",
            "error": error.to_string(),
        }),
    }
}

fn regex_request(query: &str, case_sensitive: bool) -> SearchRequest {
    SearchRequest {
        query: query.into(),
        mode: SearchMode::Regex,
        include_paths: Vec::new(),
        exclude_paths: Vec::new(),
        focus_paths: Vec::new(),
        max_results: Some(100),
        max_tokens: Some(8_000),
        context_lines: Some(1),
        case_sensitive,
        all_occurrences: false,
        cursor: None,
    }
}

async fn profile_context_matrix(
    services: &Services,
    iterations: usize,
) -> AnyResult<(Value, Value)> {
    let base = context_request(false);
    let constrained = context_request(true);
    let base_evaluation = services.context_evaluation(base.clone()).await?;
    let constrained_evaluation = services.context_evaluation(constrained.clone()).await?;
    let mut known = constrained.clone();
    known.known_hashes = constrained_evaluation
        .response
        .receipt
        .fragment_hashes
        .clone();
    let known_evaluation = services.context_evaluation(known.clone()).await?;

    let mut trace = Vec::new();
    let mut shapes = serde_json::Map::new();
    for (label, request, first) in [
        ("realistic_task", base, base_evaluation),
        ("constraint_heavy", constrained, constrained_evaluation),
        ("known_hash_replay", known, known_evaluation),
    ] {
        let mut durations = Vec::with_capacity(iterations);
        let mut latest = first;
        trace.push(latest.primitive_keys.clone());
        for _ in 0..iterations {
            let started = Instant::now();
            latest = services.context_evaluation(request.clone()).await?;
            durations.push(started.elapsed());
            trace.push(latest.primitive_keys.clone());
            black_box(&latest);
        }
        shapes.insert(
            label.into(),
            json!({
                "fragments": latest.response.fragments.len(),
                "selected_paths": latest.response.fragments.iter()
                    .map(|fragment| fragment.path.as_str())
                    .collect::<Vec<_>>(),
                "known_hashes": request.known_hashes.len(),
                "coverage": latest.response.coverage,
                "phases": latest.phases,
                "phase_timings_ms": latest.timings,
                "primitive_calls": latest.primitive_keys.len(),
                "timing_ms": TimingStats::from_durations(durations),
            }),
        );
    }
    Ok((Value::Object(shapes), summarize_reuse(&trace)))
}

fn context_request(constrained: bool) -> ContextRequest {
    let include_paths = if constrained {
        vec!["src/gateway/**".into()]
    } else {
        Vec::new()
    };
    let must_include_paths = if constrained {
        vec![
            "src/gateway/server.ts".into(),
            "src/gateway/auth-resolve.ts".into(),
        ]
    } else {
        Vec::new()
    };
    let symbols = if constrained {
        vec!["startGatewayServer".into(), "resolveGatewayAuth".into()]
    } else {
        Vec::new()
    };
    ContextRequest {
        task: "Trace gateway server startup, configuration loading, and authentication handling"
            .into(),
        token_budget: 4_000,
        include_paths: include_paths.clone(),
        must_include_paths,
        must_include_symbols: symbols.clone(),
        max_fragments: Some(12),
        focus_paths: include_paths,
        strict_focus_paths: false,
        minimum_fragments_per_focus_path: None,
        focus_symbols: symbols,
        exclude_paths: Vec::new(),
        known_hashes: Vec::new(),
        prior_repository_generation: None,
        base_revision: None,
        changed_paths: Vec::new(),
        strict_changed_paths: false,
    }
}

fn summarize_reuse(requests: &[Vec<RetrievalPrimitiveKey>]) -> Value {
    let keys = requests.iter().flatten().collect::<Vec<_>>();
    let unique = keys
        .iter()
        .map(|key| (key.kind.as_str(), key.key_blake3.as_str()))
        .collect::<HashSet<_>>();
    let within_request_reuses = requests
        .iter()
        .map(|request| {
            let unique = request
                .iter()
                .map(|key| (key.kind.as_str(), key.key_blake3.as_str()))
                .collect::<HashSet<_>>();
            request.len().saturating_sub(unique.len())
        })
        .sum::<usize>();
    let mut calls_by_kind = BTreeMap::new();
    let mut unique_by_kind: BTreeMap<&str, HashSet<&str>> = BTreeMap::new();
    let mut last_seen = HashMap::<(&str, &str), usize>::new();
    let mut reuse_distances = Vec::new();
    let mut immediate_reuses = 0usize;
    let mut reuse_distances_by_kind = BTreeMap::<&str, Vec<usize>>::new();
    for (index, key) in keys.iter().enumerate() {
        *calls_by_kind.entry(key.kind.as_str()).or_insert(0usize) += 1;
        unique_by_kind
            .entry(key.kind.as_str())
            .or_default()
            .insert(key.key_blake3.as_str());
        let identity = (key.kind.as_str(), key.key_blake3.as_str());
        if let Some(previous) = last_seen.insert(identity, index) {
            let distance = index.saturating_sub(previous);
            immediate_reuses += usize::from(distance == 1);
            reuse_distances.push(distance);
            reuse_distances_by_kind
                .entry(key.kind.as_str())
                .or_default()
                .push(distance);
        }
    }
    let unique_by_kind = unique_by_kind
        .into_iter()
        .map(|(kind, values)| (kind, values.len()))
        .collect::<BTreeMap<_, _>>();
    let reuse_distance_by_kind = reuse_distances_by_kind
        .into_iter()
        .map(|(kind, distances)| (kind, percentile_summary(distances)))
        .collect::<BTreeMap<_, _>>();
    json!({
        "requests": requests.len(),
        "calls": keys.len(),
        "unique_generation_scoped_keys": unique.len(),
        "exact_reuses": keys.len().saturating_sub(unique.len()),
        "within_request_exact_reuses": within_request_reuses,
        "cross_request_exact_reuses": keys.len()
            .saturating_sub(unique.len())
            .saturating_sub(within_request_reuses),
        "immediate_reuses": immediate_reuses,
        "reuse_distance_calls": percentile_summary(reuse_distances),
        "reuse_distance_by_kind": reuse_distance_by_kind,
        "calls_by_kind": calls_by_kind,
        "unique_by_kind": unique_by_kind,
        "interpretation": "controlled identical-request replay; temporal distances describe this harness and are not production cache-hit evidence",
    })
}

fn percentile_summary(mut values: Vec<usize>) -> Value {
    if values.is_empty() {
        return json!({
            "samples": 0,
            "p50": null,
            "p95": null,
            "max": null,
        });
    }
    values.sort_unstable();
    let percentile = |numerator: usize| {
        let index = values.len().saturating_mul(numerator).saturating_sub(1) / 100;
        values[index.min(values.len() - 1)]
    };
    json!({
        "samples": values.len(),
        "p50": percentile(50),
        "p95": percentile(95),
        "max": values.last(),
    })
}

async fn profile_reads(
    services: &Services,
    path: &str,
    lines: usize,
    bytes: usize,
    iterations: usize,
) -> AnyResult<Value> {
    let shallow = read_request(path, Some(1), Some(lines.min(32)), 4_096);
    let deep_start = lines.saturating_sub(31).max(1);
    let deep = read_request(path, Some(deep_start), Some(lines), 4_096);
    let complete = read_request(path, Some(1), Some(lines.min(128)), 32_000);
    let truncated = read_request(path, None, None, 128);

    let shallow_profile = measure_read(services, shallow, iterations).await?;
    let deep_profile = measure_read(services, deep, iterations).await?;
    let complete_profile = measure_read(services, complete, iterations).await?;
    let truncated_profile = measure_read(services, truncated, iterations).await?;
    let first_page = truncated_profile.1;
    let continuation = match first_page.continuation_cursor.clone() {
        Some(cursor) => Some(
            services
                .read(ReadRequest {
                    path: path.into(),
                    start_line: None,
                    end_line: None,
                    symbol: None,
                    heading: None,
                    heading_occurrence: None,
                    continuation_cursor: Some(cursor),
                    max_tokens: Some(128),
                    expected_hash: None,
                })
                .await?,
        ),
        None => None,
    };
    Ok(json!({
        "path": path,
        "file_bytes": bytes,
        "file_lines": lines,
        "shallow_complete": read_report(shallow_profile),
        "deep_complete": read_report(deep_profile),
        "larger_complete": read_report(complete_profile),
        "truncated_first_page": read_report((truncated_profile.0, first_page)),
        "continuation": continuation.as_ref().map(read_outcome),
    }))
}

async fn measure_read(
    services: &Services,
    request: ReadRequest,
    iterations: usize,
) -> AnyResult<(TimingStats, leantoken::ReadResponse)> {
    services.read(request.clone()).await?;
    let mut durations = Vec::with_capacity(iterations);
    let mut latest = None;
    for _ in 0..iterations {
        let started = Instant::now();
        let response = services.read(request.clone()).await?;
        durations.push(started.elapsed());
        black_box(&response);
        latest = Some(response);
    }
    Ok((
        TimingStats::from_durations(durations),
        latest.expect("iterations are positive"),
    ))
}

fn read_report(profile: (TimingStats, leantoken::ReadResponse)) -> Value {
    json!({
        "timing_ms": profile.0,
        "response": read_outcome(&profile.1),
    })
}

fn read_outcome(response: &leantoken::ReadResponse) -> Value {
    json!({
        "status": response.status,
        "target_start_line": response.target_start_line,
        "target_end_line": response.target_end_line,
        "returned_start_line": response.returned_start_line,
        "returned_end_line": response.returned_end_line,
        "truncated": response.truncated,
        "index_stale": response.index_stale,
        "source_tokens": response.meta.source_tokens,
    })
}

fn read_request(
    path: &str,
    start_line: Option<usize>,
    end_line: Option<usize>,
    max_tokens: usize,
) -> ReadRequest {
    ReadRequest {
        path: path.into(),
        start_line,
        end_line,
        symbol: None,
        heading: None,
        heading_occurrence: None,
        continuation_cursor: None,
        max_tokens: Some(max_tokens),
        expected_hash: None,
    }
}

impl TimingStats {
    fn from_durations(durations: Vec<Duration>) -> Self {
        let mut milliseconds = durations
            .iter()
            .map(|duration| duration.as_secs_f64() * 1_000.0)
            .collect::<Vec<_>>();
        milliseconds.sort_by(f64::total_cmp);
        Self {
            samples: milliseconds.len(),
            p50_ms: percentile(&milliseconds, 50),
            p95_ms: percentile(&milliseconds, 95),
            mean_ms: milliseconds.iter().sum::<f64>() / milliseconds.len() as f64,
        }
    }
}

fn percentile(values: &[f64], percentile: usize) -> f64 {
    let index = values.len().saturating_mul(percentile).saturating_sub(1) / 100;
    values[index.min(values.len().saturating_sub(1))]
}

fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mandatory_trigrams_are_folded_unique_and_include_boundaries() {
        assert_eq!(
            mandatory_ascii_trigrams(&["Abcd", "abc"]),
            vec!["abc".to_owned(), "bcd".to_owned()]
        );
    }

    #[test]
    fn reuse_summary_reports_temporal_distance() {
        let keys = [
            RetrievalPrimitiveKey {
                kind: "symbol".into(),
                key_blake3: "a".into(),
            },
            RetrievalPrimitiveKey {
                kind: "reference".into(),
                key_blake3: "b".into(),
            },
            RetrievalPrimitiveKey {
                kind: "symbol".into(),
                key_blake3: "a".into(),
            },
        ];

        let summary = summarize_reuse(&[keys.to_vec()]);

        assert_eq!(summary["exact_reuses"], 1);
        assert_eq!(summary["within_request_exact_reuses"], 1);
        assert_eq!(summary["cross_request_exact_reuses"], 0);
        assert_eq!(summary["immediate_reuses"], 0);
        assert_eq!(summary["reuse_distance_calls"]["p50"], 2);
    }

    #[tokio::test]
    async fn columnsize_zero_variant_preserves_word_and_regex_search() {
        let root = tempfile::tempdir().expect("repository");
        let database = tempfile::tempdir().expect("database");
        fs::write(
            root.path().join("source.rs"),
            "pub fn needle_symbol() { needle_symbol(); }\n// gateway authentication configuration startup\n",
        )
        .expect("source");
        let config = Config::discover(root.path(), Some(database.path().join("index.sqlite")))
            .expect("config");
        drop(Storage::open(&config.database_path).expect("default schema"));
        assert_eq!(
            detect_fts_columnsize(&config.database_path).expect("default columnsize"),
            1
        );
        initialize_columnsize_zero_schema(&config).expect("columnsize schema");
        assert_eq!(
            detect_fts_columnsize(&config.database_path).expect("columnsize"),
            0
        );
        let services = Services::open(config).expect("services");
        services.index(false).await.expect("index");

        let text = services
            .search(SearchRequest {
                query: "needle_symbol".into(),
                mode: SearchMode::Text,
                include_paths: Vec::new(),
                exclude_paths: Vec::new(),
                focus_paths: Vec::new(),
                max_results: Some(10),
                max_tokens: Some(1_000),
                context_lines: Some(0),
                case_sensitive: true,
                all_occurrences: false,
                cursor: None,
            })
            .await
            .expect("word search");
        let regex = services
            .search(regex_request(r"needle_symbol\s*\(", true))
            .await
            .expect("regex search");

        assert!(!text.hits.is_empty());
        assert!(!regex.hits.is_empty());

        let ranking =
            profile_lexical_ranking(&database.path().join("index.sqlite"), 1).expect("ranking");
        assert_eq!(ranking["gateway"]["current_bm25_with_tiebreak"]["rows"], 1);
        for variant in ["rank_with_tiebreak", "rank_only", "rank_first_hydration"] {
            assert_eq!(ranking["gateway"][variant]["same_order_as_current"], true);
            assert_eq!(ranking["gateway"][variant]["same_set_as_current"], true);
            assert_eq!(ranking["gateway"][variant]["same_scores_as_current"], true);
        }
    }
}
