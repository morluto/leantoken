impl Storage {
    pub(crate) fn record_token_savings(
        &self,
        tokenizer: &str,
        observation: TokenSavingsObservation<'_>,
    ) -> Result<bool> {
        let TokenSavingsObservation {
            operation,
            baseline_source_tokens,
            meta,
            classification,
            expected_hash_not_modified,
            expected_hash_suppressed_source_tokens,
        } = observation;
        if expected_hash_not_modified && classification != TokenSavingsRequestClass::HashSuppressed
        {
            return Err(Error::InternalFailure(
                "expected-hash suppression requires hash-suppressed classification".into(),
            ));
        }
        let response_baseline_requests = i64::from(baseline_source_tokens.is_some());
        let response_baseline_source_tokens = usize_to_i64(baseline_source_tokens.unwrap_or(0))?;
        let response_source_tokens = usize_to_i64(meta.source_tokens)?;
        let tracked_requests = i64::from(
            response_baseline_requests != 0 && classification == TokenSavingsRequestClass::Useful,
        );
        let baseline_source_tokens = if tracked_requests == 0 {
            0
        } else {
            response_baseline_source_tokens
        };
        let emitted_source_tokens = if tracked_requests == 0 {
            0
        } else {
            response_source_tokens
        };
        let estimated_source_tokens_saved = if tracked_requests == 0 {
            0
        } else {
            baseline_source_tokens
                .saturating_sub(emitted_source_tokens)
                .max(0)
        };
        let path_and_metadata_tokens = usize_to_i64(meta.path_and_metadata_tokens)?;
        let protocol_tokens = usize_to_i64(meta.protocol_tokens)?;
        let total_response_tokens = usize_to_i64(meta.total_response_tokens)?;
        let receipt_suppressed_exact = usize_to_i64(meta.receipt_suppressed_exact)?;
        let receipt_suppressed_overlap = usize_to_i64(meta.receipt_suppressed_overlap)?;
        let expected_hash_not_modified_responses = i64::from(expected_hash_not_modified);
        let expected_hash_suppressed_source_tokens = if expected_hash_not_modified {
            usize_to_i64(expected_hash_suppressed_source_tokens)?
        } else {
            0
        };
        let useful_requests = i64::from(classification == TokenSavingsRequestClass::Useful);
        let incomplete_requests = i64::from(classification == TokenSavingsRequestClass::Incomplete);
        let unsupported_requests =
            i64::from(classification == TokenSavingsRequestClass::Unsupported);
        let hash_suppressed_requests =
            i64::from(classification == TokenSavingsRequestClass::HashSuppressed);
        let conn = match self.writer.try_lock() {
            Ok(conn) => conn,
            Err(std::sync::TryLockError::WouldBlock) => return Ok(false),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        };
        conn.busy_timeout(Duration::ZERO)?;
        let result = conn.execute(
            "INSERT INTO token_savings(
                 tokenizer, operation, tracked_requests,
                 response_tracked_requests, response_baseline_requests,
                 baseline_source_tokens, response_baseline_source_tokens,
                 emitted_source_tokens,
                 estimated_source_tokens_saved, response_source_tokens,
                 path_and_metadata_tokens,
                 protocol_tokens, total_response_tokens,
                 receipt_suppressed_exact, receipt_suppressed_overlap,
                 expected_hash_not_modified_responses,
                 expected_hash_suppressed_source_tokens,
                 useful_requests, incomplete_requests,
                 unsupported_requests, hash_suppressed_requests
             ) VALUES (
                 ?1, ?2, ?3, 1, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                 ?14, ?15, ?16, ?17, ?18, ?19, ?20
             )
             ON CONFLICT(tokenizer, operation) DO UPDATE SET
                 tracked_requests = CASE
                     WHEN tracked_requests > 9223372036854775807 - excluded.tracked_requests
                         THEN 9223372036854775807
                     ELSE tracked_requests + excluded.tracked_requests
                 END,
                 response_tracked_requests = CASE
                     WHEN response_tracked_requests = 9223372036854775807
                         THEN response_tracked_requests
                     ELSE response_tracked_requests + 1
                 END,
                 response_baseline_requests = CASE
                     WHEN response_baseline_requests > 9223372036854775807 - excluded.response_baseline_requests
                         THEN 9223372036854775807
                     ELSE response_baseline_requests + excluded.response_baseline_requests
                 END,
                 baseline_source_tokens = CASE
                     WHEN baseline_source_tokens > 9223372036854775807 - excluded.baseline_source_tokens
                         THEN 9223372036854775807
                     ELSE baseline_source_tokens + excluded.baseline_source_tokens
                 END,
                 response_baseline_source_tokens = CASE
                     WHEN response_baseline_source_tokens > 9223372036854775807 - excluded.response_baseline_source_tokens
                         THEN 9223372036854775807
                     ELSE response_baseline_source_tokens + excluded.response_baseline_source_tokens
                 END,
                 emitted_source_tokens = CASE
                     WHEN emitted_source_tokens > 9223372036854775807 - excluded.emitted_source_tokens
                         THEN 9223372036854775807
                     ELSE emitted_source_tokens + excluded.emitted_source_tokens
                 END,
                 estimated_source_tokens_saved = CASE
                     WHEN estimated_source_tokens_saved > 9223372036854775807 - excluded.estimated_source_tokens_saved
                         THEN 9223372036854775807
                     ELSE estimated_source_tokens_saved + excluded.estimated_source_tokens_saved
                 END,
                 response_source_tokens = CASE
                     WHEN response_source_tokens > 9223372036854775807 - excluded.response_source_tokens
                         THEN 9223372036854775807
                     ELSE response_source_tokens + excluded.response_source_tokens
                 END,
                 path_and_metadata_tokens = CASE
                     WHEN path_and_metadata_tokens > 9223372036854775807 - excluded.path_and_metadata_tokens
                         THEN 9223372036854775807
                     ELSE path_and_metadata_tokens + excluded.path_and_metadata_tokens
                 END,
                 protocol_tokens = CASE
                     WHEN protocol_tokens > 9223372036854775807 - excluded.protocol_tokens
                         THEN 9223372036854775807
                     ELSE protocol_tokens + excluded.protocol_tokens
                 END,
                 total_response_tokens = CASE
                     WHEN total_response_tokens > 9223372036854775807 - excluded.total_response_tokens
                         THEN 9223372036854775807
                     ELSE total_response_tokens + excluded.total_response_tokens
                 END,
                 receipt_suppressed_exact = CASE
                     WHEN receipt_suppressed_exact > 9223372036854775807 - excluded.receipt_suppressed_exact
                         THEN 9223372036854775807
                     ELSE receipt_suppressed_exact + excluded.receipt_suppressed_exact
                 END,
                 receipt_suppressed_overlap = CASE
                     WHEN receipt_suppressed_overlap > 9223372036854775807 - excluded.receipt_suppressed_overlap
                         THEN 9223372036854775807
                     ELSE receipt_suppressed_overlap + excluded.receipt_suppressed_overlap
                 END,
                 expected_hash_not_modified_responses = CASE
                     WHEN expected_hash_not_modified_responses > 9223372036854775807 - excluded.expected_hash_not_modified_responses
                         THEN 9223372036854775807
                     ELSE expected_hash_not_modified_responses + excluded.expected_hash_not_modified_responses
                 END,
                 expected_hash_suppressed_source_tokens = CASE
                     WHEN expected_hash_suppressed_source_tokens > 9223372036854775807 - excluded.expected_hash_suppressed_source_tokens
                         THEN 9223372036854775807
                     ELSE expected_hash_suppressed_source_tokens + excluded.expected_hash_suppressed_source_tokens
                 END,
                 useful_requests = CASE
                     WHEN useful_requests > 9223372036854775807 - excluded.useful_requests
                         THEN 9223372036854775807
                     ELSE useful_requests + excluded.useful_requests
                 END,
                 incomplete_requests = CASE
                     WHEN incomplete_requests > 9223372036854775807 - excluded.incomplete_requests
                         THEN 9223372036854775807
                     ELSE incomplete_requests + excluded.incomplete_requests
                 END,
                 unsupported_requests = CASE
                     WHEN unsupported_requests > 9223372036854775807 - excluded.unsupported_requests
                         THEN 9223372036854775807
                     ELSE unsupported_requests + excluded.unsupported_requests
                 END,
                 hash_suppressed_requests = CASE
                     WHEN hash_suppressed_requests > 9223372036854775807 - excluded.hash_suppressed_requests
                         THEN 9223372036854775807
                     ELSE hash_suppressed_requests + excluded.hash_suppressed_requests
                 END",
            params![
                tokenizer,
                operation.as_str(),
                tracked_requests,
                response_baseline_requests,
                baseline_source_tokens,
                response_baseline_source_tokens,
                emitted_source_tokens,
                estimated_source_tokens_saved,
                response_source_tokens,
                path_and_metadata_tokens,
                protocol_tokens,
                total_response_tokens,
                receipt_suppressed_exact,
                receipt_suppressed_overlap,
                expected_hash_not_modified_responses,
                expected_hash_suppressed_source_tokens,
                useful_requests,
                incomplete_requests,
                unsupported_requests,
                hash_suppressed_requests,
            ],
        );
        let restore_timeout = conn.busy_timeout(DEFAULT_BUSY_TIMEOUT);
        match result {
            Ok(_) => {
                restore_timeout?;
                Ok(true)
            }
            Err(rusqlite::Error::SqliteFailure(error, _))
                if matches!(
                    error.code,
                    rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                ) =>
            {
                restore_timeout?;
                Ok(false)
            }
            Err(error) => {
                restore_timeout?;
                Err(error.into())
            }
        }
    }

    pub(crate) fn record_service_failure(
        &self,
        tokenizer: &str,
        operation: TokenAccountingOperation,
        error_category: &str,
    ) -> Result<bool> {
        let conn = match self.writer.try_lock() {
            Ok(conn) => conn,
            Err(std::sync::TryLockError::WouldBlock) => return Ok(false),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        };
        conn.busy_timeout(Duration::ZERO)?;
        let result = conn.execute(
            "INSERT INTO service_failures(
                 tokenizer, operation, error_category, failed_requests
             ) VALUES (?1, ?2, ?3, 1)
             ON CONFLICT(tokenizer, operation, error_category) DO UPDATE SET
                 failed_requests = CASE
                     WHEN failed_requests = 9223372036854775807
                         THEN failed_requests
                     ELSE failed_requests + 1
                 END",
            params![tokenizer, operation.as_str(), error_category],
        );
        let restore_timeout = conn.busy_timeout(DEFAULT_BUSY_TIMEOUT);
        match result {
            Ok(_) => {
                restore_timeout?;
                Ok(true)
            }
            Err(rusqlite::Error::SqliteFailure(error, _))
                if matches!(
                    error.code,
                    rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                ) =>
            {
                restore_timeout?;
                Ok(false)
            }
            Err(error) => {
                restore_timeout?;
                Err(error.into())
            }
        }
    }

    pub(crate) fn token_savings(
        &self,
        tokenizer: &str,
    ) -> Result<HashMap<String, TokenSavingsRecord>> {
        self.begin_read()?.token_savings(tokenizer)
    }
}
