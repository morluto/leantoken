use std::time::{SystemTime, UNIX_EPOCH};

use super::*;
use crate::receipt::StoredReceipt;

pub(in crate::mcp) const RECEIPT_RESOURCE_MEDIA_TYPE: &str =
    "application/vnd.leantoken.retrieval-receipt+json;version=1";
pub(in crate::mcp) const RECEIPT_RESOURCE_TEMPLATE: &str = "leantoken://receipt/v1/{receipt_id}";
const RECEIPT_RESOURCE_PREFIX: &str = "leantoken://receipt/v1/";

#[derive(Serialize)]
struct ReceiptResource<'a> {
    schema_version: u8,
    kind: &'static str,
    uri: &'a str,
    receipt_id: &'a str,
    repository_id: &'a str,
    repository_identity: &'a str,
    repository_generation: u64,
    created_unix_millis: i64,
    expires_unix_millis: i64,
    evidence_count: usize,
    evidence: Vec<ReceiptResourceEvidence<'a>>,
    complete: bool,
    source_free: bool,
}

#[derive(Serialize)]
struct ReceiptResourceEvidence<'a> {
    path: &'a str,
    start_line: usize,
    end_line: usize,
    content_hash: &'a str,
    exact_only: bool,
}

pub(in crate::mcp) fn receipt_uri(receipt_id: &str) -> String {
    format!("{RECEIPT_RESOURCE_PREFIX}{receipt_id}")
}

pub(in crate::mcp) fn receipt_resource_link(receipt_id: &str) -> Resource {
    Resource::new(receipt_uri(receipt_id), "retrieval_receipt")
        .with_title("LeanToken retrieval receipt")
        .with_description("Complete source-free evidence identity receipt")
        .with_mime_type(RECEIPT_RESOURCE_MEDIA_TYPE)
}

fn parse_receipt_uri(uri: &str) -> Option<&str> {
    let receipt_id = uri.strip_prefix(RECEIPT_RESOURCE_PREFIX)?;
    if receipt_id.len() != crate::receipt::RECEIPT_ID_RESPONSE_RESERVE.len()
        || !receipt_id.starts_with('r')
        || !receipt_id[1..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    Some(receipt_id)
}

fn now_unix_millis() -> Result<i64, ErrorData> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ErrorData::internal_error("system clock precedes the Unix epoch", None))?
        .as_millis();
    i64::try_from(millis)
        .map_err(|_| ErrorData::internal_error("system clock exceeds supported range", None))
}

fn resource_not_found(uri: &str) -> ErrorData {
    ErrorData::resource_not_found(
        "retrieval receipt resource not found",
        Some(serde_json::json!({"uri": uri})),
    )
}

fn resource_value<'a>(
    receipt: &'a StoredReceipt,
    repository_id: &'a str,
    uri: &'a str,
) -> ReceiptResource<'a> {
    ReceiptResource {
        schema_version: 1,
        kind: "retrieval_receipt",
        uri,
        receipt_id: &receipt.receipt_id,
        repository_id,
        repository_identity: &receipt.repository_identity,
        repository_generation: receipt.repository_generation,
        created_unix_millis: receipt.created_unix_millis,
        expires_unix_millis: receipt.expires_unix_millis,
        evidence_count: receipt.evidence.len(),
        evidence: receipt
            .evidence
            .iter()
            .map(|evidence| ReceiptResourceEvidence {
                path: &evidence.path,
                start_line: evidence.start_line,
                end_line: evidence.end_line,
                content_hash: &evidence.content_hash,
                exact_only: evidence.exact_only,
            })
            .collect(),
        complete: receipt.complete,
        source_free: true,
    }
}

impl LeanTokenMcp {
    pub(in crate::mcp) fn list_receipt_resources(
        &self,
        protocol: Option<ProtocolVersion>,
    ) -> ListResourcesResult {
        let result = ListResourcesResult::default();
        if protocol
            .as_ref()
            .is_some_and(|version| version >= &ProtocolVersion::V_2026_07_28)
        {
            result.with_ttl_ms(0).with_cache_scope(CacheScope::Private)
        } else {
            result
        }
    }

    pub(in crate::mcp) fn list_receipt_resource_templates(
        &self,
        protocol: Option<ProtocolVersion>,
    ) -> ListResourceTemplatesResult {
        let template = ResourceTemplate::new(RECEIPT_RESOURCE_TEMPLATE, "retrieval_receipt")
            .with_title("LeanToken retrieval receipt")
            .with_description("Read a receipt URI returned by a LeanToken retrieval tool")
            .with_mime_type(RECEIPT_RESOURCE_MEDIA_TYPE);
        let result = ListResourceTemplatesResult::with_all_items(vec![template]);
        if protocol
            .as_ref()
            .is_some_and(|version| version >= &ProtocolVersion::V_2026_07_28)
        {
            result.with_ttl_ms(0).with_cache_scope(CacheScope::Private)
        } else {
            result
        }
    }

    pub(in crate::mcp) async fn read_receipt_resource(
        &self,
        uri: String,
        protocol: Option<ProtocolVersion>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let receipt_id = parse_receipt_uri(&uri)
            .ok_or_else(|| resource_not_found(&uri))?
            .to_owned();
        let _admission = self.resource_read_admission.try_admit().map_err(|_| {
            ErrorData::internal_error(
                "receipt resource capacity is exhausted; retry shortly",
                Some(serde_json::json!({
                    "category": "retrieval_capacity_exhausted",
                    "retry_after_ms": 500,
                })),
            )
        })?;
        let state = self.services.get();
        let services = match state {
            McpServiceState::Ready { services, .. } => services,
            _ => {
                return Err(ErrorData::internal_error(
                    "repository storage is unavailable",
                    None,
                ));
            }
        };
        let now = now_unix_millis()?;
        let repository_id = services.repository_id();
        let receipt =
            tokio::task::spawn_blocking(move || services.read_stored_receipt(&receipt_id, now))
                .await
                .map_err(|error| {
                    tracing::error!(%error, "receipt resource read task failed");
                    ErrorData::internal_error("retrieval receipt read failed", None)
                })?
                .map_err(|error| match error {
                    crate::Error::UnknownReceipt(_) => resource_not_found(&uri),
                    other => {
                        tracing::error!(%other, "receipt resource read failed");
                        ErrorData::internal_error("retrieval receipt read failed", None)
                    }
                })?;
        let text = serde_json::to_string(&resource_value(&receipt, &repository_id, &uri)).map_err(
            |error| {
                tracing::error!(%error, "receipt resource serialization failed");
                ErrorData::internal_error("retrieval receipt serialization failed", None)
            },
        )?;
        let content = ResourceContents::text(text, uri).with_mime_type(RECEIPT_RESOURCE_MEDIA_TYPE);
        let mut result = ReadResourceResult::new(vec![content]);
        if protocol
            .as_ref()
            .is_some_and(|version| version >= &ProtocolVersion::V_2026_07_28)
        {
            result = result.with_ttl_ms(0).with_cache_scope(CacheScope::Private);
        }
        Ok(ReadResourceResponse::Complete(result))
    }
}
