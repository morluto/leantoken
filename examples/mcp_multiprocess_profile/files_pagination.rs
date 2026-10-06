use std::{collections::HashSet, error::Error};

use serde_json::{Value, json};

use super::{
    MAX_FIXTURE_FILES, normalize_response, response_fingerprint, successful_tool_response,
};

const MAX_CURSOR_BYTES: usize = 11_000;

#[derive(Debug, Clone)]
pub(super) struct ValidatedFilesCursor {
    encoded: String,
    canonical: String,
}

#[derive(Debug)]
pub(super) struct FilesBaseline {
    pub(super) response: Value,
    pub(super) cursor: Option<ValidatedFilesCursor>,
    pub(super) validation_requests: usize,
}

/// Validate continuations through their owning MCP session before comparing identities.
pub(super) fn validate_baseline(
    first: Value,
    fixture_files: usize,
    mut follow: impl FnMut(&str) -> Result<Value, Box<dyn Error>>,
) -> Result<FilesBaseline, Box<dyn Error>> {
    if !(1..=MAX_FIXTURE_FILES).contains(&fixture_files) {
        return Err("files pagination fixture count is outside its bound".into());
    }
    let repository = first
        .pointer("/result/structuredContent/meta/repository_id")
        .and_then(Value::as_str)
        .filter(|identity| {
            identity.len() == 32
                && identity
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .ok_or("files pagination repository identity is unrecognized")?
        .to_owned();
    let mut next_index = 0;
    let first_cursor = validate_page(&first, &repository, fixture_files, &mut next_index)?;
    let first_page_len = next_index;
    let mut cursor = first_cursor.clone();
    let mut seen = HashSet::new();
    let mut pages = blake3::Hasher::new();
    hash_page(&mut pages, first.clone(), cursor.as_deref())?;
    let mut validation_requests = 0;
    let mut first_continuation_fingerprint = None;

    while let Some(encoded) = cursor {
        if !seen.insert(*blake3::hash(encoded.as_bytes()).as_bytes()) {
            return Err("files pagination cursor repeated without completing the fixture".into());
        }
        let response = follow(&encoded)?;
        validation_requests += 1;
        cursor = validate_page(&response, &repository, fixture_files, &mut next_index)?;
        if validation_requests == 1 {
            first_continuation_fingerprint =
                Some(response_fingerprint(&normalize_response(response.clone())));
        }
        hash_page(&mut pages, response, cursor.as_deref())?;
    }
    if next_index != fixture_files {
        return Err("files pagination ended before the complete fixture inventory".into());
    }

    // Replaying the first continuation must reproduce its immutable page and raw cursor.
    if let Some(encoded) = first_cursor.as_deref() {
        let repeated = follow(encoded)?;
        validation_requests += 1;
        let mut repeated_index = first_page_len;
        validate_page(&repeated, &repository, fixture_files, &mut repeated_index)?;
        if Some(response_fingerprint(&normalize_response(repeated)))
            != first_continuation_fingerprint
        {
            return Err("files continuation changed when replayed in the same session".into());
        }
    }

    let cursor = first_cursor.map(|encoded| ValidatedFilesCursor {
        encoded,
        canonical: format!("validated-files-pages-v1:{}", pages.finalize().to_hex()),
    });
    Ok(FilesBaseline {
        response: normalize(first, cursor.as_ref()),
        cursor,
        validation_requests,
    })
}

fn validate_page(
    response: &Value,
    repository: &str,
    fixture_files: usize,
    next_index: &mut usize,
) -> Result<Option<String>, Box<dyn Error>> {
    if !successful_tool_response(response) {
        return Err("MCP files continuation did not succeed".into());
    }
    let payload = response
        .pointer("/result/structuredContent")
        .ok_or("files pagination requires structured content")?;
    let meta = payload
        .get("meta")
        .ok_or("files pagination metadata missing")?;
    if meta.get("repository_id").and_then(Value::as_str) != Some(repository)
        || meta.get("repository_generation").and_then(Value::as_u64) != Some(1)
        || meta.get("index_scope").and_then(Value::as_str) != Some("full")
        || meta.get("token_count_exact").and_then(Value::as_bool) != Some(true)
    {
        return Err("files pagination changed repository, generation, scope or exactness".into());
    }
    if let Some(content) = response
        .pointer("/result/content")
        .and_then(Value::as_array)
    {
        for item in content {
            if let Some(text) = item.get("text").and_then(Value::as_str)
                && let Ok(text_payload) = serde_json::from_str::<Value>(text)
                && text_payload.get("entries").is_some()
                && &text_payload != payload
            {
                return Err("files pagination text and structured payloads disagree".into());
            }
        }
    }
    let entries = payload
        .get("entries")
        .and_then(Value::as_array)
        .filter(|entries| !entries.is_empty())
        .ok_or("files pagination did not advance its inventory")?;
    for entry in entries {
        if *next_index >= fixture_files
            || entry.get("kind").and_then(Value::as_str) != Some("file")
            || entry.get("path").and_then(Value::as_str)
                != Some(format!("file_{:05}.rs", *next_index).as_str())
        {
            return Err("files pagination skipped, duplicated or reordered fixture paths".into());
        }
        *next_index += 1;
    }
    let cursor = match meta.get("next_cursor") {
        None | Some(Value::Null) => None,
        Some(Value::String(encoded))
            if !encoded.is_empty() && encoded.len() <= MAX_CURSOR_BYTES =>
        {
            Some(encoded.clone())
        }
        _ => {
            return Err(
                "files pagination cursor is missing its bounded string representation".into(),
            );
        }
    };
    if cursor.is_some() && *next_index == fixture_files {
        return Err("files pagination offers a continuation beyond the complete fixture".into());
    }
    Ok(cursor)
}

fn hash_page(
    pages: &mut blake3::Hasher,
    mut response: Value,
    encoded: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    if let Some(encoded) = encoded {
        replace_cursor(&mut response, encoded, &json!(true));
    }
    let bytes = serde_json::to_vec(&normalize_response(response))?;
    pages.update(&(bytes.len() as u64).to_le_bytes());
    pages.update(&bytes);
    Ok(())
}

pub(super) fn normalize(mut response: Value, cursor: Option<&ValidatedFilesCursor>) -> Value {
    if let Some(cursor) = cursor {
        replace_cursor(&mut response, &cursor.encoded, &json!(cursor.canonical));
    }
    normalize_response(response)
}

fn replace_cursor(response: &mut Value, encoded: &str, replacement: &Value) {
    fn replace(payload: &mut Value, encoded: &str, replacement: &Value) {
        if payload.pointer("/meta/next_cursor").and_then(Value::as_str) == Some(encoded) {
            payload["meta"]["next_cursor"] = replacement.clone();
        }
    }
    if let Some(payload) = response.pointer_mut("/result/structuredContent") {
        replace(payload, encoded, replacement);
    }
    if let Some(content) = response
        .pointer_mut("/result/content")
        .and_then(Value::as_array_mut)
    {
        for item in content {
            if let Some(Value::String(text)) = item.get_mut("text")
                && let Ok(mut payload) = serde_json::from_str::<Value>(text)
            {
                replace(&mut payload, encoded, replacement);
                if let Ok(serialized) = serde_json::to_string(&payload) {
                    *text = serialized;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(repository: char, start: usize, end: usize, cursor: Option<&str>) -> Value {
        let payload = json!({
            "entries": (start..end).map(|index| json!({
                "path": format!("file_{index:05}.rs"), "kind": "file",
                "language": "rust", "size_bytes": 40 + index,
            })).collect::<Vec<_>>(),
            "meta": {
                "repository_id": repository.to_string().repeat(32),
                "repository_generation": 1, "index_scope": "full",
                "token_count_exact": true, "next_cursor": cursor,
            },
        });
        json!({"jsonrpc":"2.0", "id":1, "result": {
            "structuredContent":payload,
            "content":[{"type":"text", "text":serde_json::to_string(&payload).unwrap()}],
        }})
    }

    fn synchronize_text(response: &mut Value) {
        response["result"]["content"][0]["text"] =
            json!(serde_json::to_string(&response["result"]["structuredContent"]).unwrap());
    }

    #[test]
    fn independent_identities_compare_only_after_complete_validated_pagination() {
        let baseline = |repository, encoded| {
            let first = reply(repository, 0, 2, Some(encoded));
            let second = reply(repository, 2, 4, None);
            validate_baseline(first, 4, |cursor| {
                assert_eq!(cursor, encoded);
                Ok(second.clone())
            })
            .unwrap()
        };
        let first = baseline('a', "session-a");
        let second = baseline('b', "session-b");
        assert_eq!(first.validation_requests, 2);
        assert_eq!(first.response, second.response);
    }

    #[test]
    fn complete_unpaginated_fixture_keeps_existing_response_and_request_count() {
        let first = reply('a', 0, 4, None);
        let baseline =
            validate_baseline(first.clone(), 4, |_| panic!("unexpected follow")).unwrap();
        assert_eq!(baseline.validation_requests, 0);
        assert!(baseline.cursor.is_none());
        assert_eq!(baseline.response, normalize_response(first));
    }

    #[test]
    fn missing_duplicate_skipped_empty_and_extra_pages_are_rejected() {
        for next in [
            reply('a', 2, 3, None),
            reply('a', 1, 4, None),
            reply('a', 3, 4, None),
            reply('a', 2, 2, None),
            reply('a', 2, 4, Some("extra")),
        ] {
            assert!(
                validate_baseline(reply('a', 0, 2, Some("start")), 4, |_| Ok(next.clone()))
                    .is_err()
            );
        }
    }

    #[test]
    fn cursor_loops_and_service_rejections_fail_before_normalization() {
        let mut calls = 0;
        assert!(
            validate_baseline(reply('a', 0, 2, Some("loop")), 6, |_| {
                calls += 1;
                Ok(reply('a', 2, 4, Some("loop")))
            })
            .is_err()
        );
        assert_eq!(calls, 1);
        assert!(
            validate_baseline(reply('a', 0, 2, Some("invalid")), 4, |_| {
                Err("owning service rejected stale or malformed cursor".into())
            })
            .is_err()
        );
    }

    #[test]
    fn changed_generation_repository_scope_or_exactness_is_rejected() {
        for (key, value) in [
            ("repository_generation", json!(2)),
            ("repository_id", json!("b".repeat(32))),
            ("index_scope", json!("partial")),
            ("token_count_exact", json!(false)),
        ] {
            let mut next = reply('a', 2, 4, None);
            next["result"]["structuredContent"]["meta"][key] = value;
            synchronize_text(&mut next);
            assert!(
                validate_baseline(reply('a', 0, 2, Some("start")), 4, |_| Ok(next.clone()))
                    .is_err()
            );
        }
    }

    #[test]
    fn malformed_and_oversized_cursors_and_fixture_bounds_are_rejected() {
        for value in [
            json!(false),
            json!(""),
            json!("x".repeat(MAX_CURSOR_BYTES + 1)),
        ] {
            let mut first = reply('a', 0, 2, Some("start"));
            first["result"]["structuredContent"]["meta"]["next_cursor"] = value;
            synchronize_text(&mut first);
            assert!(validate_baseline(first, 4, |_| panic!("malformed cursor followed")).is_err());
        }
        for files in [0, MAX_FIXTURE_FILES + 1] {
            assert!(validate_baseline(reply('a', 0, 2, None), files, |_| panic!()).is_err());
        }
    }

    #[test]
    fn contradictory_payloads_and_nondeterministic_continuations_are_rejected() {
        let mut first = reply('a', 0, 2, Some("start"));
        first["result"]["content"][0]["text"] =
            reply('a', 0, 2, Some("different"))["result"]["content"][0]["text"].clone();
        assert!(validate_baseline(first, 4, |_| panic!("contradictory cursor followed")).is_err());
        let mut calls = 0;
        assert!(
            validate_baseline(reply('a', 0, 2, Some("start")), 4, |_| {
                calls += 1;
                let mut next = reply('a', 2, 4, None);
                if calls == 2 {
                    next["result"]["structuredContent"]["entries"][0]["size_bytes"] = json!(999);
                    synchronize_text(&mut next);
                }
                Ok(next)
            })
            .is_err()
        );
    }

    #[test]
    fn later_page_content_drift_and_unknown_warm_cursors_remain_significant() {
        let first = reply('a', 0, 2, Some("start"));
        let baseline = validate_baseline(first.clone(), 4, |_| Ok(reply('a', 2, 4, None))).unwrap();
        let mut changed = reply('a', 2, 4, None);
        changed["result"]["structuredContent"]["entries"][0]["size_bytes"] = json!(999);
        synchronize_text(&mut changed);
        let drifted = validate_baseline(first, 4, |_| Ok(changed.clone())).unwrap();
        assert_ne!(baseline.response, drifted.response);
        for cursor in [Some("unknown"), None] {
            let response = reply('a', 0, 2, cursor);
            assert_eq!(
                normalize(response.clone(), None),
                normalize_response(response.clone())
            );
            assert_ne!(
                normalize(response, baseline.cursor.as_ref()),
                baseline.response
            );
        }
    }
}
