use similar::TextDiff;

use crate::model::{
    ReadDeltaBaseSource, ReadDeltaFallback, ReadDeltaOutcome, ReadDeltaPersistenceFallback,
    ReadDeltaReceipt, ReadResponse,
};
use crate::read_delta::{MAX_READ_DELTA_BASE_BYTES, ReadDeltaBase};
use crate::storage::ArtifactStorage;
use crate::tokens::Tokenizer;
use crate::{Error, Result};

use super::read::NewReadTarget;

pub(super) struct ReadDeltaEvaluation {
    pub delta: Option<String>,
    pub receipt: ReadDeltaReceipt,
}

pub(super) struct ReadDeltaInput<'a> {
    pub repository_id: &'a str,
    pub artifacts: &'a ArtifactStorage,
    pub base_artifact_id: Option<&'a str>,
    pub path: &'a str,
    pub target: &'a NewReadTarget,
    pub expected_hash: Option<&'a str>,
    pub response: &'a ReadResponse,
    pub current_content: &'a str,
    pub full_tokens: usize,
    pub tokenizer: Tokenizer,
}

pub(super) fn evaluate(input: ReadDeltaInput<'_>) -> Result<ReadDeltaEvaluation> {
    let ReadDeltaInput {
        repository_id,
        artifacts,
        base_artifact_id,
        path,
        target,
        expected_hash,
        response,
        current_content,
        full_tokens,
        tokenizer,
    } = input;
    let target_key = target_key(repository_id, path, target);
    let base = base_artifact_id
        .map(|id| artifacts.load_read_base(repository_id, id))
        .transpose()?;
    if let Some(base) = &base
        && base.target_key != target_key
    {
        return Err(Error::InvalidInput {
            field: "delta_base_artifact_id",
            reason: "belongs to another read target",
        });
    }

    let mut persistence_fallback_reason = if response.truncated {
        Some(ReadDeltaPersistenceFallback::CurrentTruncated)
    } else if current_content.len() > MAX_READ_DELTA_BASE_BYTES {
        Some(ReadDeltaPersistenceFallback::ContentTooLarge)
    } else {
        None
    };
    let head_artifact_id = if persistence_fallback_reason.is_none() {
        let current = ReadDeltaBase {
            target_key: target_key.clone(),
            content_hash: response.content_hash.clone(),
            content: current_content.to_owned(),
            generation: response.meta.repository_generation,
            target_start_line: response.target_start_line,
            target_end_line: response.target_end_line,
            returned_start_line: response.returned_start_line,
            returned_end_line: response.returned_end_line,
        };
        match artifacts.persist_read_base(repository_id, &current) {
            Ok(id) => Some(id),
            Err(error) => {
                tracing::warn!(%error, "read artifact capture was skipped");
                persistence_fallback_reason = Some(ReadDeltaPersistenceFallback::StorageCapacity);
                None
            }
        }
    } else {
        None
    };

    let mut outcome = ReadDeltaOutcome::Full;
    let mut delta = None;
    let mut delta_tokens = None;
    let mut avoided_tokens = 0;
    let mut fallback_reason = None;
    if expected_hash == Some(response.content_hash.as_str())
        || base
            .as_ref()
            .is_some_and(|base| base.content_hash == response.content_hash)
    {
        outcome = ReadDeltaOutcome::NotModified;
        delta_tokens = Some(0);
        avoided_tokens = full_tokens;
    } else if response.truncated {
        fallback_reason = Some(ReadDeltaFallback::CurrentTruncated);
    } else if current_content.len() > MAX_READ_DELTA_BASE_BYTES {
        fallback_reason = Some(ReadDeltaFallback::ContentTooLarge);
    } else if let Some(base) = &base {
        if target_coordinates_changed(base, response) {
            fallback_reason = Some(ReadDeltaFallback::TargetChanged);
        } else {
            let candidate = TextDiff::from_lines(base.content.as_str(), current_content)
                .unified_diff()
                .context_radius(3)
                .header(
                    &format!(
                        "base/{}:{}-{}",
                        response.path, response.returned_start_line, response.returned_end_line
                    ),
                    &format!(
                        "head/{}:{}-{}",
                        response.path, response.returned_start_line, response.returned_end_line
                    ),
                )
                .to_string();
            let candidate_tokens = tokenizer.count(&candidate);
            if candidate_tokens < full_tokens {
                outcome = ReadDeltaOutcome::Delta;
                delta_tokens = Some(candidate_tokens);
                avoided_tokens = full_tokens.saturating_sub(candidate_tokens);
                delta = Some(candidate);
            } else {
                fallback_reason = Some(ReadDeltaFallback::DeltaNotSmaller);
            }
        }
    } else {
        fallback_reason = Some(ReadDeltaFallback::BaseUnavailable);
    }

    Ok(ReadDeltaEvaluation {
        delta,
        receipt: ReadDeltaReceipt {
            target_key,
            base_hash: base.as_ref().map(|base| base.content_hash.clone()),
            base_artifact_id: base_artifact_id.map(str::to_owned),
            head_hash: response.content_hash.clone(),
            head_artifact_id: head_artifact_id.clone(),
            base_generation: base.as_ref().map(|base| base.generation),
            base_source: base.as_ref().map(|_| ReadDeltaBaseSource::Artifact),
            head_generation: response.meta.repository_generation,
            outcome,
            full_tokens,
            delta_tokens,
            avoided_tokens,
            artifact_capture_fallback_reason: persistence_fallback_reason,
            fallback_reason,
        },
    })
}

fn target_key(repository_id: &str, path: &str, target: &NewReadTarget) -> String {
    let mut hasher = blake3::Hasher::new();
    for value in [repository_id, path] {
        hasher.update(value.as_bytes());
        hasher.update(&[0]);
    }
    match target {
        NewReadTarget::Symbol(symbol) => {
            hasher.update(b"symbol\0");
            hasher.update(symbol.as_bytes());
        }
        NewReadTarget::Heading { name, occurrence } => {
            hasher.update(b"heading\0");
            hasher.update(name.as_bytes());
            hasher.update(&[0]);
            hasher.update(
                &u64::try_from(occurrence.get())
                    .unwrap_or(u64::MAX)
                    .to_le_bytes(),
            );
        }
        NewReadTarget::Lines { start, end } => {
            hasher.update(b"lines\0");
            hasher.update(&u64::try_from(start.get()).unwrap_or(u64::MAX).to_le_bytes());
            hasher.update(
                &end.and_then(|line| u64::try_from(line).ok())
                    .unwrap_or(u64::MAX)
                    .to_le_bytes(),
            );
        }
    }
    hasher.finalize().to_hex().to_string()
}

fn target_coordinates_changed(base: &ReadDeltaBase, head: &ReadResponse) -> bool {
    base.target_start_line != head.target_start_line
        || base.target_end_line != head.target_end_line
        || base.returned_start_line != head.returned_start_line
        || base.returned_end_line != head.returned_end_line
}
