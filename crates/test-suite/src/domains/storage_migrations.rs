//! Authentic forward-only compatibility tests, independently selectable by owner.

use std::{fs, path::Path};

use leantoken::storage::{ChunkInput, IndexedFile, Storage};
use rusqlite_migration::{M, Migrations};

// Compile the authoritative declarations for fixture construction only. The
// normal opener still comes from the product dependency; no SQL is copied.
// Bootstrap accounting constants are intentionally unused by prefix generation.
#[allow(dead_code)]
#[path = "../../../../src/storage/schema.rs"]
mod production_schema;
use leantoken_test_support::Sandbox;
use production_schema::migration_test_support as migrations;
use rusqlite::{Connection, types::Value};
use serde::Deserialize;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/storage-migrations");
const MAX_FIXTURE_BYTES: u64 = 1024 * 1024;
const CONTENT: &str = "pub fn legacy_widget() { println!(\"migración\"); }\n";

#[derive(Deserialize)]
struct Compatibility {
    latest_migration: usize,
    latest_schema_version: i64,
    released_through: usize,
    release_fixtures_reviewed_through: usize,
    release_anchors: Vec<ReleaseAnchor>,
    allowed_schema_differences: Vec<String>,
    migrations: Vec<MigrationIdentity>,
}

#[derive(Deserialize)]
struct MigrationIdentity {
    version: usize,
    name: String,
    blake3: String,
}

#[derive(Deserialize)]
struct ReleaseAnchor {
    version: String,
    metadata_blake3: String,
}

#[derive(Deserialize)]
struct ReleaseFixture {
    version: String,
    tag: String,
    commit: String,
    archive_sha256: String,
    binary_sha256: String,
    schema_source_sha256: String,
    cargo_lock_sha256: String,
    fixture_blake3: String,
    fixture_bytes: u64,
    fixture: String,
    wal_fixture: Option<String>,
    wal_bytes: u64,
    wal_blake3: Option<String>,
    migration_version: usize,
    schema_version: i64,
    sqlite_writer_version_number: u32,
    command: Vec<String>,
    supported_until: String,
}

fn compatibility() -> Compatibility {
    serde_json::from_slice(
        &fs::read(Path::new(FIXTURES).join("compatibility.json")).expect("compatibility manifest"),
    )
    .expect("valid compatibility manifest")
}

fn connection(database: &Path) -> Connection {
    let connection = Connection::open(database).expect("fixture connection");
    connection
        .execute_batch("PRAGMA foreign_keys = ON; PRAGMA max_page_count = 256;")
        .expect("bound the disposable migration fixture to 1 MiB");
    connection
}

fn scalar(connection: &Connection, sql: &str) -> i64 {
    connection.query_row(sql, [], |row| row.get(0)).expect(sql)
}

fn has_table(connection: &Connection, table: &str) -> bool {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
            [table],
            |row| row.get(0),
        )
        .expect("table capability")
}

fn has_column(connection: &Connection, table: &str, column: &str) -> bool {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_xinfo(?1) WHERE name=?2)",
            [table, column],
            |row| row.get(0),
        )
        .expect("column capability")
}

fn rows(connection: &Connection, sql: &str) -> Vec<Vec<Value>> {
    let mut statement = connection.prepare(sql).expect(sql);
    let columns = statement.column_count();
    let values = statement
        .query_map([], |row| {
            (0..columns).map(|column| row.get(column)).collect()
        })
        .expect(sql)
        .take(1025)
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect(sql);
    assert!(
        values.len() <= 1024,
        "fixture query exceeded 1024 rows: {sql}"
    );
    values
}

fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn object_names(connection: &Connection, kind: &str) -> Vec<String> {
    connection
        .prepare("SELECT name FROM sqlite_schema WHERE type=?1 ORDER BY name")
        .expect("schema inventory")
        .query_map([kind], |row| row.get(0))
        .expect("schema names")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("schema names")
}

fn canonical_sql(name: &str, sql: &str) -> String {
    let mut normalized = String::new();
    let mut quoted = None;
    let mut pending_space = false;
    for character in sql.trim().chars() {
        if let Some(delimiter) = quoted {
            normalized.push(character);
            if character == delimiter {
                quoted = None;
            }
        } else if character.is_whitespace() {
            pending_space = true;
        } else {
            if pending_space
                && !matches!(character, ',' | '(' | ')' | ';')
                && !normalized.ends_with('(')
                && !normalized.ends_with(',')
            {
                normalized.push(' ');
            }
            pending_space = false;
            normalized.push(character);
            if matches!(character, '\'' | '"' | '`') {
                quoted = Some(character);
            }
        }
    }
    if !matches!(name, "token_savings" | "meta") {
        return normalized;
    }
    // Released source-only savings tables acquired additive columns via ALTER
    // TABLE. Physical column order is the sole reviewed compatibility exception.
    // Sort top-level declarations, preserving each definition and composite key
    // order, so checks/defaults/constraints cannot disappear in normalization.
    // The runtime database_identity field likewise precedes scope metadata in
    // v0.1.27 and follows it in fresh databases; all declarations remain checked.
    let start = normalized.find('(').expect("savings table declarations");
    let end = normalized.rfind(')').expect("savings table declarations");
    let mut depth = 0;
    let mut beginning = start + 1;
    let mut definitions = Vec::new();
    for (offset, character) in normalized[start + 1..end].char_indices() {
        match character {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                definitions.push(normalized[beginning..start + 1 + offset].trim());
                beginning = start + 2 + offset;
            }
            _ => {}
        }
    }
    definitions.push(normalized[beginning..end].trim());
    definitions.sort_unstable();
    format!("{}({})", &normalized[..start], definitions.join(","))
}

fn schema_inventory(connection: &Connection) -> Vec<String> {
    let mut inventory = Vec::new();
    let objects = connection
        .prepare("SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name")
        .expect("schema objects")
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })
        .expect("schema objects")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("schema objects");
    assert!(
        objects.len() <= 256,
        "schema inventory exceeded fixture bound"
    );
    for (kind, name, table, sql) in objects {
        inventory.push(format!(
            "object {kind} {name} {table} {:?}",
            sql.map(|sql| canonical_sql(&name, &sql))
        ));
    }
    for table in object_names(connection, "table") {
        let table_literal = table.replace('\'', "''");
        let columns = if matches!(table.as_str(), "token_savings" | "meta") {
            format!(
                "SELECT name,type,\"notnull\",dflt_value,pk,hidden FROM pragma_table_xinfo('{table_literal}') ORDER BY name"
            )
        } else {
            format!("SELECT * FROM pragma_table_xinfo('{table_literal}') ORDER BY cid")
        };
        inventory.push(format!("columns {table} {:?}", rows(connection, &columns)));
        inventory.push(format!(
            "indexes {table} {:?}",
            rows(connection, &format!("SELECT name,\"unique\",origin,partial FROM pragma_index_list('{table_literal}') ORDER BY name"))
        ));
        inventory.push(format!(
            "foreign_keys {table} {:?}",
            rows(
                connection,
                &format!("PRAGMA foreign_key_list({})", quote(&table))
            )
        ));
    }
    for index in object_names(connection, "index") {
        inventory.push(format!(
            "index_columns {index} {:?}",
            rows(
                connection,
                &format!("PRAGMA index_xinfo({})", quote(&index))
            )
        ));
    }
    inventory.push(format!(
        "user_version {}",
        scalar(connection, "PRAGMA user_version")
    ));
    inventory.push(format!(
        "schema_version {}",
        scalar(connection, "SELECT schema_version FROM meta")
    ));
    inventory
}

fn logical_snapshot(connection: &Connection) -> Vec<String> {
    object_names(connection, "table")
        .into_iter()
        .map(|table| {
            let mut values = rows(connection, &format!("SELECT * FROM {}", quote(&table)))
                .into_iter()
                .map(|row| format!("{row:?}"))
                .collect::<Vec<_>>();
            values.sort_unstable();
            format!(
                "{table}: rows={}, blake3={}",
                values.len(),
                migrations::artifact_digest(format!("{values:?}").as_bytes())
            )
        })
        .collect()
}

fn assert_inventory(actual: &[String], expected: &[String], directory: &Path) {
    if actual != expected {
        fs::write(directory.join("actual-schema.txt"), actual.join("\n"))
            .expect("actual schema evidence");
        fs::write(directory.join("expected-schema.txt"), expected.join("\n"))
            .expect("expected schema evidence");
        let mismatch = actual
            .iter()
            .zip(expected)
            .position(|(actual, expected)| actual != expected);
        panic!(
            "migration schema differs at inventory entry {mismatch:?}; bounded schema inventories and database preserved at {}",
            directory.display()
        );
    }
}

#[derive(Default, Clone, Copy)]
struct Seeded {
    core: bool,
    candidates: bool,
    paths: bool,
    receipt: bool,
    delta: bool,
    coverage: bool,
    savings: bool,
    failures: bool,
}

fn seed(connection: &Connection) -> Seeded {
    if !has_table(connection, "files") {
        return Seeded::default();
    }
    connection
        .execute(
            "INSERT INTO files(id,path,language,structurally_complete,size_bytes,modified_ns,content_hash,generation) VALUES (1,'src/legacy.rs','rust',1,?1,NULL,'historical-content',7)",
            [CONTENT.len() as i64],
        )
        .expect("historical file");
    connection
        .execute(
            "INSERT INTO chunks(id,file_id,content,start_line,end_line,start_byte,end_byte,token_count) VALUES (1,1,?1,1,1,0,?2,9)",
            rusqlite::params![CONTENT, CONTENT.len() as i64],
        )
        .expect("historical chunk and FTS content");
    connection
        .execute_batch(
            "INSERT INTO symbols(id,file_id,name,kind,parent,signature,start_line,end_line,start_byte,end_byte) VALUES (1,1,'legacy_widget','function',NULL,'pub fn legacy_widget()',1,1,0,50);
             INSERT INTO symbol_refs(id,file_id,name,kind,role,enclosing_symbol,start_line,end_line,start_byte,end_byte) VALUES (1,1,'println','function','reference','legacy_widget',1,1,0,50);
             INSERT INTO imports(id,file_id,raw_target,resolved_path,line) VALUES (1,1,'std::fmt',NULL,1);
             UPDATE meta SET index_version=7,repository_generation=7,config_hash='historical-config';",
        )
        .expect("historical syntax and metadata");
    let candidates = has_table(connection, "import_candidates");
    if candidates {
        connection
            .execute_batch("INSERT INTO import_candidates VALUES (1,'src/候補.rs',0);")
            .expect("candidate");
    }
    if has_column(connection, "meta", "repository_identity") {
        connection.execute_batch("UPDATE meta SET repository_root='/historical/repository',repository_identity='historical-repository';").expect("repository identity");
    }
    let paths = has_table(connection, "path_entries");
    if paths {
        connection
            .execute_batch(
                "INSERT INTO path_entries VALUES ('src',1,0,NULL),('src/legacy.rs',2,1,1);",
            )
            .expect("path projection");
    }
    let mut seeded = Seeded {
        core: true,
        candidates,
        paths,
        ..Seeded::default()
    };
    seed_capabilities(connection, &mut seeded);
    seeded
}

fn seed_capabilities(connection: &Connection, seeded: &mut Seeded) {
    if has_table(connection, "retrieval_receipts") {
        let evidence_bytes = if has_column(connection, "retrieval_receipt_evidence", "exact_only") {
            72
        } else {
            64
        };
        connection.execute_batch(
            "INSERT INTO retrieval_receipts(id,repository_identity,repository_generation,created_unix_millis,last_access_unix_millis,expires_unix_millis,access_sequence,logical_bytes) VALUES (1,'historical-repository',7,0,0,9223372036854775807,1,128);
             INSERT INTO retrieval_receipt_evidence(receipt_id,ordinal,path,start_line,end_line,content_hash,semantic_signature,logical_bytes) VALUES (1,0,'src/legacy.rs',0,1,'historical-content',NULL,64);",
        ).expect("historical receipt and nullable evidence");
        connection
            .execute(
                "UPDATE retrieval_receipt_evidence SET logical_bytes=?1",
                [evidence_bytes],
            )
            .expect("evidence size");
        connection
            .execute(
                "UPDATE retrieval_receipts SET evidence_count=1,evidence_bytes=?1",
                [evidence_bytes],
            )
            .expect("receipt counters");
        connection.execute("UPDATE retrieval_receipt_usage SET next_access_sequence=1,evidence_count=1,evidence_bytes=?1", [evidence_bytes]).expect("usage counters");
        seeded.receipt = true;
    }
    if has_table(connection, "read_delta_bases") {
        connection.execute_batch(
            "INSERT INTO read_delta_bases(target_key,content_hash,repository_generation,target_start_line,target_end_line,returned_start_line,returned_end_line,content,created_unix_millis,last_access_unix_millis,expires_unix_millis,access_sequence,logical_bytes) VALUES ('historical-target','historical-content',7,1,1,1,1,'migración',0,0,9223372036854775807,1,96);
             UPDATE read_delta_base_usage SET next_access_sequence=1;",
        ).expect("historical delta base and usage triggers");
        seeded.delta = true;
    }
    if has_table(connection, "query_coverage_receipts") {
        connection.execute_batch(
            "INSERT INTO query_coverage_receipts(repository_identity,repository_generation,config_hash,semantics_version,predicate_json,predicate_blake3,partition_blake3,partition_file_count,match_count,result_blake3,created_unix_millis,last_access_unix_millis,expires_unix_millis,access_sequence,logical_bytes) VALUES ('historical-repository',7,'historical-config',1,'{}',printf('%064d',0),printf('%064d',0),1,1,printf('%064d',0),0,0,9223372036854775807,1,144);
             UPDATE query_coverage_receipt_usage SET next_access_sequence=1;",
        ).expect("historical query coverage receipt");
        seeded.coverage = true;
    }
    if has_table(connection, "token_savings") {
        connection.execute_batch("INSERT INTO token_savings(tokenizer,operation,tracked_requests,baseline_source_tokens,emitted_source_tokens,estimated_source_tokens_saved) VALUES ('cl100k_base','search',2,100,20,80);").expect("historical savings");
        seeded.savings = true;
    }
    if has_table(connection, "service_failures") {
        connection.execute_batch("INSERT INTO service_failures VALUES ('cl100k_base','search','historical-error',1);").expect("historical service failure");
        seeded.failures = true;
    }
}

fn assert_seeded(connection: &Connection, seeded: Seeded) {
    if seeded.core {
        assert_seeded_core(connection);
    }
    if seeded.candidates {
        assert_eq!(
            scalar(
                connection,
                "SELECT count(*) FROM import_candidates WHERE candidate_path='src/候補.rs' AND priority=0"
            ),
            1
        );
    }
    if seeded.paths {
        assert_eq!(
            scalar(
                connection,
                "SELECT count(*) FROM path_entries WHERE path='src/legacy.rs' AND file_id=1"
            ),
            1
        );
    }
    if seeded.receipt {
        assert_seeded_receipt(connection);
    }
    if seeded.delta {
        assert_seeded_delta(connection);
    }
    if seeded.coverage {
        assert_seeded_coverage(connection);
    }
    if seeded.savings {
        assert_seeded_savings(connection);
    }
    if seeded.failures {
        assert_eq!(
            scalar(
                connection,
                "SELECT failed_requests FROM service_failures WHERE error_category='historical-error'"
            ),
            1
        );
    }
}

fn assert_seeded_core(connection: &Connection) {
    assert_eq!(
        scalar(
            connection,
            "SELECT count(*) FROM files WHERE path='src/legacy.rs' AND content_hash='historical-content' AND modified_ns IS NULL"
        ),
        1
    );
    assert_eq!(
        scalar(
            connection,
            "SELECT count(*) FROM chunks WHERE content LIKE '%migración%' AND token_count=9"
        ),
        1
    );
    assert_eq!(
        scalar(
            connection,
            "SELECT count(*) FROM symbols WHERE name='legacy_widget' AND parent IS NULL"
        ),
        1
    );
    assert_eq!(
        scalar(
            connection,
            "SELECT count(*) FROM symbol_refs WHERE name='println' AND role='reference'"
        ),
        1
    );
    assert_eq!(
        scalar(
            connection,
            "SELECT count(*) FROM imports WHERE raw_target='std::fmt' AND resolved_path IS NULL"
        ),
        1
    );
    assert_eq!(
        scalar(connection, "SELECT repository_generation FROM meta"),
        7
    );
}

fn assert_seeded_receipt(connection: &Connection) {
    let evidence_bytes = if has_column(connection, "retrieval_receipt_evidence", "exact_only") {
        72
    } else {
        64
    };
    assert_eq!(
        scalar(
            connection,
            "SELECT logical_bytes FROM retrieval_receipt_evidence"
        ),
        evidence_bytes
    );
    assert_eq!(
        scalar(connection, "SELECT evidence_bytes FROM retrieval_receipts"),
        evidence_bytes
    );
    assert_eq!(
        scalar(
            connection,
            "SELECT evidence_bytes FROM retrieval_receipt_usage"
        ),
        evidence_bytes
    );
    assert_eq!(
        scalar(
            connection,
            "SELECT receipt_count FROM retrieval_receipt_usage"
        ),
        1
    );
    assert_eq!(
        scalar(
            connection,
            "SELECT receipt_bytes FROM retrieval_receipt_usage"
        ),
        128
    );
    assert_eq!(
        scalar(
            connection,
            "SELECT evidence_count FROM retrieval_receipt_usage"
        ),
        1
    );
    assert_eq!(
        scalar(
            connection,
            "SELECT next_access_sequence FROM retrieval_receipt_usage"
        ),
        1
    );
}

fn assert_seeded_delta(connection: &Connection) {
    assert_eq!(
        scalar(connection, "SELECT base_count FROM read_delta_base_usage"),
        1
    );
    assert_eq!(
        scalar(connection, "SELECT base_bytes FROM read_delta_base_usage"),
        96
    );
    assert_eq!(
        scalar(
            connection,
            "SELECT count(*) FROM read_delta_bases WHERE content='migración' AND target_start_line=1"
        ),
        1
    );
}

fn assert_seeded_coverage(connection: &Connection) {
    assert_eq!(
        scalar(
            connection,
            "SELECT receipt_count FROM query_coverage_receipt_usage"
        ),
        1
    );
    assert_eq!(
        scalar(
            connection,
            "SELECT logical_bytes FROM query_coverage_receipt_usage"
        ),
        144
    );
    assert_eq!(
        scalar(
            connection,
            "SELECT match_count FROM query_coverage_receipts"
        ),
        1
    );
}

fn assert_seeded_savings(connection: &Connection) {
    assert_eq!(
        scalar(
            connection,
            "SELECT tracked_requests FROM token_savings WHERE operation='search'"
        ),
        2
    );
    assert_eq!(
        scalar(
            connection,
            "SELECT baseline_source_tokens FROM token_savings WHERE operation='search'"
        ),
        100
    );
    if has_column(connection, "token_savings", "response_tracked_requests") {
        assert_eq!(
            scalar(
                connection,
                "SELECT response_tracked_requests FROM token_savings WHERE operation='search'"
            ),
            0
        );
        assert_eq!(
            scalar(
                connection,
                "SELECT expected_hash_suppressed_source_tokens FROM token_savings WHERE operation='search'"
            ),
            0
        );
    }
}

fn assert_integrity(connection: &Connection) {
    assert_eq!(
        rows(connection, "PRAGMA integrity_check"),
        vec![vec![Value::Text("ok".into())]]
    );
    assert!(rows(connection, "PRAGMA foreign_key_check").is_empty());
    assert_eq!(scalar(connection, "PRAGMA foreign_keys"), 1);
    let journal: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("journal mode");
    assert_eq!(journal, "wal");
    for (table, column) in [
        ("chunks_fts_word", "content"),
        ("chunks_fts_trigram", "content"),
        ("symbols_fts_trigram", "name"),
        ("symbol_refs_fts_trigram", "name"),
    ] {
        connection
            .execute_batch(&format!(
                "INSERT INTO {table}({table}, rank) VALUES ('integrity-check', 1);"
            ))
            .expect(column);
    }
}

fn assert_constraints(connection: &Connection) {
    connection
        .execute_batch("SAVEPOINT constraint_probe;")
        .expect("constraint savepoint");
    for sql in [
        "UPDATE retrieval_receipt_usage SET receipt_count=-1",
        "UPDATE read_delta_base_usage SET base_bytes=-1",
        "UPDATE query_coverage_receipt_usage SET namespace='short'",
        "INSERT INTO chunks(file_id,content,start_line,end_line,start_byte,end_byte) VALUES (-1,'orphan',1,1,0,1)",
    ] {
        assert!(
            connection.execute_batch(sql).is_err(),
            "historical constraint disappeared: {sql}"
        );
    }
    connection
        .execute_batch("ROLLBACK TO constraint_probe; RELEASE constraint_probe;")
        .expect("rollback constraint probes");
}

fn assert_quota_totals(connection: &Connection) {
    for sql in [
        "SELECT receipt_count=(SELECT count(*) FROM retrieval_receipts) AND receipt_bytes=(SELECT coalesce(sum(logical_bytes),0) FROM retrieval_receipts) AND evidence_count=(SELECT count(*) FROM retrieval_receipt_evidence) AND evidence_bytes=(SELECT coalesce(sum(logical_bytes),0) FROM retrieval_receipt_evidence) FROM retrieval_receipt_usage WHERE id=1",
        "SELECT base_count=(SELECT count(*) FROM read_delta_bases) AND base_bytes=(SELECT coalesce(sum(logical_bytes),0) FROM read_delta_bases) FROM read_delta_base_usage WHERE id=1",
        "SELECT receipt_count=(SELECT count(*) FROM query_coverage_receipts) AND logical_bytes=(SELECT coalesce(sum(logical_bytes),0) FROM query_coverage_receipts) FROM query_coverage_receipt_usage WHERE id=1",
    ] {
        assert_eq!(
            scalar(connection, sql),
            1,
            "quota projection differs from authoritative logical rows"
        );
    }
}

fn assert_public_reads(storage: &Storage, seeded: Seeded) {
    if !seeded.core {
        return;
    }
    let file = storage
        .find_file("src/legacy.rs")
        .expect("find upgraded file")
        .expect("historical file survives");
    assert_eq!(file.language.as_deref(), Some("rust"));
    assert_eq!(file.size_bytes, CONTENT.len() as u64);
    assert_eq!(file.content_hash, "historical-content");
    assert_eq!(
        storage.get_chunks_for_file(file.id, 10).expect("chunks")[0].content,
        CONTENT
    );
    assert_eq!(
        storage.get_symbols_for_file(file.id, 10).expect("symbols")[0].name,
        "legacy_widget"
    );
    assert_eq!(
        storage
            .get_references_for_file(file.id, 10)
            .expect("references")[0]
            .name,
        "println"
    );
    assert_eq!(
        storage.get_imports_for_file(file.id, 10).expect("imports")[0].raw_target,
        "std::fmt"
    );
    assert_eq!(
        storage
            .search_word("migración", 10)
            .expect("word FTS")
            .len(),
        1
    );
    assert_eq!(
        storage
            .search_trigram("legacy_widget", 10)
            .expect("trigram FTS")
            .len(),
        1
    );
    assert_eq!(
        storage
            .search_symbols("gacy_wid", true, 10)
            .expect("rebuilt structural FTS")
            .len(),
        1
    );
    assert_eq!(
        storage
            .search_references("rint", true, 10)
            .expect("rebuilt reference FTS")
            .len(),
        1
    );
}

fn verify_upgrade(database: &Path, expected_schema: &[String], seeded: Seeded, directory: &Path) {
    let storage = Storage::open(database).expect("upgrade authentic historical schema");
    assert_public_reads(&storage, seeded);
    let inspect = connection(database);
    assert_seeded(&inspect, seeded);
    assert_inventory(&schema_inventory(&inspect), expected_schema, directory);
    assert_integrity(&inspect);
    assert_constraints(&inspect);
    assert_quota_totals(&inspect);
    let before = logical_snapshot(&inspect);
    drop(inspect);
    drop(storage);

    let reopened = Storage::open(database).expect("idempotent reopen");
    let inspect = connection(database);
    assert_inventory(&schema_inventory(&inspect), expected_schema, directory);
    assert_eq!(
        logical_snapshot(&inspect),
        before,
        "reopen changed logical database state"
    );
    drop(inspect);
    // A normal atomic generation replacement remains usable after migration.
    let generation = reopened
        .repository_generation()
        .expect("upgraded generation");
    let content = "postmigration replacement\n";
    reopened
        .full_reconcile(
            "post-migration",
            vec![IndexedFile {
                path: "after.txt".into(),
                language: None,
                structurally_complete: false,
                size_bytes: content.len() as u64,
                modified_ns: None,
                content_hash: leantoken::text::hash(content),
                chunks: vec![ChunkInput {
                    content: content.into(),
                    start_line: 1,
                    end_line: 1,
                    start_byte: 0,
                    end_byte: content.len(),
                    token_count: 2,
                }],
                symbols: Vec::new(),
                references: Vec::new(),
                imports: Vec::new(),
            }],
        )
        .expect("post-migration write");
    assert_eq!(
        reopened
            .repository_generation()
            .expect("written generation"),
        generation + 1
    );
    assert_eq!(
        reopened
            .list_files(10, None)
            .expect("post-migration read")
            .len(),
        1
    );
    assert!(reopened.find_file("after.txt").expect("new file").is_some());
    assert_eq!(
        reopened
            .search_word("postmigration", 10)
            .expect("post-migration FTS insert")
            .len(),
        1
    );
    assert!(
        reopened
            .search_word("migración", 10)
            .expect("post-migration FTS delete")
            .is_empty()
    );
    drop(reopened);
    assert_integrity(&connection(database));
    assert!(fs::metadata(database).expect("fixture size").len() <= MAX_FIXTURE_BYTES);
}

fn fresh_schema(directory: &Path) -> Vec<String> {
    let database = directory.join("fresh.sqlite");
    drop(Storage::open(&database).expect("fresh current schema"));
    schema_inventory(&connection(&database))
}

#[test]
fn migration_history_is_append_only_and_requires_explicit_compatibility_updates() {
    let manifest = compatibility();
    migrations::validate().expect("valid authoritative migration chain");
    let actual = migrations::definition_digests();
    assert_eq!(
        actual.len(),
        manifest.latest_migration,
        "new migration requires reviewed compatibility manifest and release anchor update"
    );
    assert_eq!(manifest.migrations.len(), actual.len());
    assert_eq!(
        manifest.released_through,
        manifest.migrations.len(),
        "release compatibility decision must cover every migration"
    );
    assert_eq!(
        manifest.release_fixtures_reviewed_through,
        actual.len(),
        "new migration requires explicit review of the immutable release fixture upgrade expectations"
    );
    assert_eq!(manifest.allowed_schema_differences.len(), 2);
    for (index, (expected, actual)) in manifest.migrations.iter().zip(actual).enumerate() {
        assert_eq!(expected.version, index + 1);
        assert_eq!(
            expected.blake3, actual,
            "released migration {} ({}) was edited/reordered; append a migration",
            expected.version, expected.name
        );
    }
    let sandbox = Sandbox::new(module_path!(), "migration_identity").expect("sandbox");
    let database = sandbox.root().join("latest.sqlite");
    drop(Storage::open(&database).expect("latest"));
    assert_eq!(
        scalar(&connection(&database), "SELECT schema_version FROM meta"),
        manifest.latest_schema_version
    );
}

#[test]
fn every_authoritative_migration_prefix_upgrades_directly_and_adjacent() {
    for prefix in 0..=migrations::migration_count() {
        for adjacent in [false, true] {
            let sandbox = Sandbox::new(module_path!(), "migration_prefix").expect("sandbox");
            fs::write(
                sandbox.root().join("migration-case.txt"),
                format!("prefix={prefix}; adjacent={adjacent}\n"),
            )
            .expect("case evidence");
            let expected = fresh_schema(sandbox.root());
            let database = sandbox.root().join("historical.sqlite");
            let mut fixture = connection(&database);
            migrations::apply_prefix(&mut fixture, prefix).expect("authoritative migration prefix");
            let seeded = seed(&fixture);
            if adjacent {
                for next in prefix + 1..=migrations::migration_count() {
                    migrations::apply_prefix(&mut fixture, next)
                        .expect("adjacent forward migration");
                    assert_seeded(&fixture, seeded);
                    assert_eq!(scalar(&fixture, "PRAGMA user_version"), next as i64);
                    assert!(rows(&fixture, "PRAGMA foreign_key_check").is_empty());
                }
            }
            drop(fixture);
            verify_upgrade(&database, &expected, seeded, sandbox.root());
        }
    }
}

fn released_provenance(anchor: &ReleaseAnchor) -> ReleaseFixture {
    let metadata_bytes = fs::read(Path::new(FIXTURES).join(format!("v{}.json", anchor.version)))
        .expect("immutable release provenance");
    assert_eq!(
        migrations::artifact_digest(&metadata_bytes),
        anchor.metadata_blake3,
        "release provenance changed; explicit compatibility fixture review is required"
    );
    let metadata: ReleaseFixture =
        serde_json::from_slice(&metadata_bytes).expect("release provenance");
    assert_eq!(metadata.version, anchor.version);
    assert_eq!(metadata.tag, format!("v{}", anchor.version));
    assert_eq!(metadata.commit.len(), 40);
    for digest in [
        &metadata.archive_sha256,
        &metadata.binary_sha256,
        &metadata.schema_source_sha256,
        &metadata.cargo_lock_sha256,
    ] {
        assert_eq!(
            digest.len(),
            64,
            "release identity must be provenance-bound"
        );
    }
    assert!(metadata.command.iter().any(|argument| argument == "index"));
    assert!(!metadata.supported_until.is_empty());
    assert!(metadata.sqlite_writer_version_number >= 3_009_000);
    metadata
}

#[test]
fn immutable_released_databases_upgrade_with_provenance_and_historical_accounting() {
    let manifest = compatibility();
    assert!(
        manifest
            .release_anchors
            .iter()
            .any(|release| release.version == "0.1.27"),
        "previous package release must remain a compatibility anchor"
    );
    for anchor in manifest.release_anchors {
        let metadata = released_provenance(&anchor);
        let version = anchor.version;
        let payload = fs::read(Path::new(FIXTURES).join(&metadata.fixture))
            .expect("immutable released database");
        assert_eq!(payload.len() as u64, metadata.fixture_bytes);
        assert!(metadata.fixture_bytes <= MAX_FIXTURE_BYTES);
        assert_eq!(
            migrations::artifact_digest(&payload),
            metadata.fixture_blake3,
            "released database bytes changed; run and review the explicit generator"
        );
        let sandbox = Sandbox::new(module_path!(), "migration_release").expect("sandbox");
        fs::write(
            sandbox.root().join("migration-case.txt"),
            format!("release={version}; commit={}\n", metadata.commit),
        )
        .expect("release evidence");
        let expected = fresh_schema(sandbox.root());
        let database = sandbox.root().join("released.sqlite");
        fs::write(&database, payload).expect("copy immutable release fixture");
        if let Some(wal_fixture) = metadata.wal_fixture {
            let wal =
                fs::read(Path::new(FIXTURES).join(wal_fixture)).expect("released committed WAL");
            assert_eq!(wal.len() as u64, metadata.wal_bytes);
            assert!(metadata.wal_bytes <= 4 * MAX_FIXTURE_BYTES);
            assert_eq!(
                Some(migrations::artifact_digest(&wal)),
                metadata.wal_blake3,
                "released WAL bytes changed"
            );
            fs::write(database.with_extension("sqlite-wal"), wal)
                .expect("copy durable released WAL before opening");
        } else {
            assert_eq!(metadata.wal_bytes, 0);
            assert!(metadata.wal_blake3.is_none());
        }
        let fixture = connection(&database);
        assert_eq!(
            scalar(&fixture, "PRAGMA user_version"),
            metadata.migration_version as i64
        );
        assert_eq!(
            scalar(&fixture, "SELECT schema_version FROM meta"),
            metadata.schema_version
        );
        assert_eq!(scalar(&fixture, "SELECT count(*) FROM files"), 1);
        let mut seeded = Seeded::default();
        seed_capabilities(&fixture, &mut seeded);
        if version == "0.1.15" {
            assert!(seeded.savings);
            assert!(
                !has_column(&fixture, "token_savings", "response_tracked_requests"),
                "legacy accounting fixture must be authentic source-only format"
            );
        }
        drop(fixture);
        // Release-created parser/index records are retained in addition to the
        // version-capability receipt/accounting seed scenario.
        let storage = Storage::open(&database).expect("open actual released database");
        assert_eq!(
            storage
                .search_symbols("legacy_widget", true, 10)
                .expect("released symbol")
                .len(),
            1
        );
        assert_eq!(
            storage
                .search_word("migración", 10)
                .expect("released FTS content")
                .len(),
            1
        );
        drop(storage);
        verify_upgrade(&database, &expected, seeded, sandbox.root());
    }
}

#[test]
fn failed_migrations_roll_back_every_forward_boundary_and_both_version_markers() {
    // A TEMP trigger rejects the real migration's metadata update only after
    // that migration has performed its DDL, FTS rebuild, or counter writes.
    // This exercises the actual production transaction without a public fault API.
    for prefix in 0..migrations::migration_count() {
        let sandbox = Sandbox::new(module_path!(), "migration_failure").expect("sandbox");
        let database = sandbox.root().join("failed.sqlite");
        fs::write(
            sandbox.root().join("migration-case.txt"),
            format!("failure-prefix={prefix}\n"),
        )
        .expect("failure case evidence");
        let mut fixture = connection(&database);
        migrations::apply_prefix(&mut fixture, prefix).expect("failure predecessor");
        let seeded = seed(&fixture);
        if prefix == 0 {
            // Initial DDL hits this intentionally incompatible historical object.
            fixture
                .execute_batch("CREATE VIEW files AS SELECT 'preserve' AS marker;")
                .expect("initial migration obstruction");
        } else {
            fixture.execute_batch("CREATE TEMP TRIGGER fail_migration BEFORE UPDATE OF schema_version ON main.meta BEGIN SELECT RAISE(ABORT,'migration fault boundary'); END;").expect("connection-local failure boundary");
        }
        let before_schema = if prefix == 0 {
            Vec::new()
        } else {
            schema_inventory(&fixture)
        };
        let before_data = logical_snapshot(&fixture);
        // The lookup-index migration does not update meta. For prefix1, migrate
        // through the following metadata update to prove both entries roll back.
        let failure = migrations::apply_prefix(&mut fixture, migrations::migration_count())
            .expect_err("failure hook must reject historical prefix");
        if prefix > 0 {
            assert!(
                failure.to_string().contains("migration fault boundary"),
                "unexpected migration failure at prefix {prefix}: {failure}"
            );
        }
        assert_eq!(scalar(&fixture, "PRAGMA user_version"), prefix as i64);
        if prefix > 0 {
            assert_inventory(&schema_inventory(&fixture), &before_schema, sandbox.root());
            assert_seeded(&fixture, seeded);
        } else {
            assert!(
                !has_table(&fixture, "meta"),
                "failed initial migration left its metadata behind"
            );
            assert_eq!(
                scalar(
                    &fixture,
                    "SELECT count(*) FROM sqlite_schema WHERE type='view' AND name='files'"
                ),
                1
            );
        }
        assert_eq!(
            logical_snapshot(&fixture),
            before_data,
            "failed migration changed historical data"
        );
        if prefix > 0 {
            fixture
                .execute_batch("DROP TRIGGER temp.fail_migration;")
                .expect("remove failure boundary");
            drop(fixture);
            let expected = fresh_schema(sandbox.root());
            verify_upgrade(&database, &expected, seeded, sandbox.root());
        }
    }
}

#[test]
fn storage_open_rolls_back_conflicting_historical_ddl_without_losing_data() {
    // These public-opener cases retain the old receipt/delta/query-receipt
    // tests' distinct DDL-collision boundary. The prefix matrix separately
    // faults after each migration's schema/data writes.
    for (prefix, obstruction) in [
        (7, "retrieval_receipts"),
        (8, "read_delta_bases"),
        (10, "query_coverage_receipt_usage"),
    ] {
        let sandbox = Sandbox::new(module_path!(), "migration_ddl_conflict").expect("sandbox");
        fs::write(
            sandbox.root().join("migration-case.txt"),
            format!("prefix={prefix}; obstruction={obstruction}\n"),
        )
        .expect("conflict case evidence");
        let database = sandbox.root().join("historical-conflict.sqlite");
        let mut historical = connection(&database);
        migrations::apply_prefix(&mut historical, prefix).expect("authentic conflict predecessor");
        let seeded = seed(&historical);
        historical.execute_batch(&format!("CREATE TABLE {}(conflicting_value TEXT); INSERT INTO {} VALUES ('preserved obstruction');", quote(obstruction), quote(obstruction))).expect("deliberate historical DDL obstruction");
        let before_schema = schema_inventory(&historical);
        let before_data = logical_snapshot(&historical);
        drop(historical);

        let error = match Storage::open(&database) {
            Err(error) => error,
            Ok(_) => panic!("normal opener must reject conflicting DDL"),
        };
        assert!(
            error.to_string().contains(obstruction),
            "unexpected migration failure: {error}"
        );
        let inspect = connection(&database);
        assert_inventory(&schema_inventory(&inspect), &before_schema, sandbox.root());
        assert_eq!(
            logical_snapshot(&inspect),
            before_data,
            "failed opener lost historical or obstruction data"
        );
        assert_seeded(&inspect, seeded);
        assert_eq!(scalar(&inspect, "PRAGMA user_version"), prefix as i64);
        inspect
            .execute_batch(&format!("DROP TABLE {};", quote(obstruction)))
            .expect("remove only the deliberate failure obstruction");
        drop(inspect);

        let expected = fresh_schema(sandbox.root());
        verify_upgrade(&database, &expected, seeded, sandbox.root());
    }
}
