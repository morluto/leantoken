use super::Services;
use crate::Result;

use crate::receipt::ReceiptEvaluation;
pub(super) use crate::receipt::{ReceiptDecision, ReceiptEvidence};

impl Services {
    pub(super) fn evaluate_receipt(
        &self,
        requested_id: Option<&str>,
        generation: u64,
        database_incarnation_id: &str,
        candidates: &[ReceiptEvidence],
    ) -> Result<ReceiptEvaluation> {
        self.artifacts.evaluate_receipt(
            &self.repository_id(),
            database_incarnation_id,
            requested_id,
            generation,
            candidates,
            true,
        )
    }

    pub(super) fn evaluate_read_receipt(
        &self,
        requested_id: Option<&str>,
        generation: u64,
        database_incarnation_id: &str,
        candidates: &[ReceiptEvidence],
    ) -> Result<ReceiptEvaluation> {
        self.artifacts.evaluate_receipt(
            &self.repository_id(),
            database_incarnation_id,
            requested_id,
            generation,
            candidates,
            false,
        )
    }
}
