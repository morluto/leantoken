use super::Services;
use crate::Result;

use crate::receipt::ReceiptEvaluation;
pub(super) use crate::receipt::{ReceiptDecision, ReceiptEvidence};

impl Services {
    pub(super) fn evaluate_receipt(
        &self,
        requested_id: Option<&str>,
        generation: u64,
        candidates: &[ReceiptEvidence],
    ) -> Result<ReceiptEvaluation> {
        self.storage
            .evaluate_receipt(requested_id, generation, candidates, true)
    }

    pub(super) fn evaluate_read_receipt(
        &self,
        requested_id: Option<&str>,
        generation: u64,
        candidates: &[ReceiptEvidence],
    ) -> Result<ReceiptEvaluation> {
        self.storage
            .evaluate_receipt(requested_id, generation, candidates, false)
    }
}
