use std::{
    env,
    io::{IsTerminal, Write},
};

use leantoken::{
    ObservedTokenSavingsReport, ResponseTokenAccountingByOperation, Result,
    TokenAccountingOperation, TokenSavingsSnapshotReport, TokenSavingsWindow,
};

const RESET: &str = "\x1b[0m";
const BOLD_CYAN: &str = "\x1b[1;36m";
const BOLD_GREEN: &str = "\x1b[1;32m";
const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const DIM: &str = "\x1b[2m";

struct DisplayRow {
    operation: &'static str,
    requests: String,
    baseline_requests: String,
    baseline: String,
    source: String,
    metadata: String,
    protocol: String,
    total: String,
    net: String,
    net_tokens: i64,
}

#[derive(Clone, Copy)]
struct Palette {
    enabled: bool,
}

impl Palette {
    fn paint(self, style: &str, text: &str) -> String {
        if self.enabled {
            format!("{style}{text}{RESET}")
        } else {
            text.to_owned()
        }
    }
}

pub(crate) fn print_report(report: &TokenSavingsSnapshotReport, json_output: bool) -> Result<()> {
    let stdout = std::io::stdout();
    let color = color_enabled(stdout.is_terminal());
    let mut output = stdout.lock();
    if json_output {
        serde_json::to_writer(&mut output, report)?;
        output.write_all(b"\n")?;
        return Ok(());
    }
    write_human_report(&mut output, &report.observed, color)?;
    writeln!(
        output,
        "Window: {}",
        match report.window {
            TokenSavingsWindow::Lifetime => "lifetime",
            TokenSavingsWindow::Delta => "snapshot delta",
        }
    )?;
    writeln!(output, "Snapshot: {}", report.snapshot)?;
    Ok(())
}

fn write_human_report(
    output: &mut impl Write,
    report: &ObservedTokenSavingsReport,
    color: bool,
) -> Result<()> {
    let palette = Palette { enabled: color };
    let accounting = &report.report.response_accounting;
    let observations = &report.observations;
    let net = format_signed_count(accounting.estimated_net_tokens_saved);
    let net_summary = if accounting.estimated_net_tokens_saved > 0 {
        format!("{net} fewer response tokens than represented source")
    } else if accounting.estimated_net_tokens_saved < 0 {
        format!(
            "{} more response tokens than represented source",
            format_count(accounting.estimated_net_tokens_saved.unsigned_abs())
        )
    } else {
        "response tokens equal represented source".into()
    };
    let net_style = match accounting.estimated_net_tokens_saved.cmp(&0) {
        std::cmp::Ordering::Greater => BOLD_GREEN,
        std::cmp::Ordering::Less => YELLOW,
        std::cmp::Ordering::Equal => DIM,
    };
    writeln!(
        output,
        "{}",
        palette.paint(BOLD_CYAN, "LeanToken Observed Token Accounting")
    )?;
    writeln!(
        output,
        "{}",
        palette.paint(DIM, "===================================")
    )?;
    writeln!(
        output,
        "{}  ({} response delta)",
        palette.paint(net_style, &net_summary),
        format_net_reduction(
            accounting.estimated_net_tokens_saved,
            accounting.baseline_source_tokens
        )
    )?;
    writeln!(
        output,
        "{} baseline  ->  {} total response",
        format_count(accounting.baseline_source_tokens),
        format_count(accounting.total_response_tokens)
    )?;
    writeln!(
        output,
        "{} source + {} metadata + {} protocol",
        format_count(accounting.response_source_tokens),
        format_count(accounting.path_and_metadata_tokens),
        format_count(accounting.protocol_tokens)
    )?;
    writeln!(
        output,
        "{} successful responses  |  {} with baselines",
        format_count(accounting.tracked_requests),
        format_count(accounting.baseline_requests)
    )?;
    writeln!(
        output,
        "Persisted observations: {} successful  |  {} failed  |  {} expected-hash not-modified",
        format_count(observations.successful_response_records),
        format_count(observations.failed_service_requests),
        format_count(observations.expected_hash_not_modified_responses)
    )?;
    writeln!(
        output,
        "Expected-hash suppression: {} represented-source tokens",
        format_count(observations.expected_hash_suppressed_source_tokens)
    )?;
    let classification = &observations.request_classification;
    writeln!(
        output,
        "Request classes: {} useful  |  {} incomplete  |  {} unsupported  |  {} hash-suppressed  |  {} unclassified  |  {} failed",
        format_count(classification.useful),
        format_count(classification.incomplete),
        format_count(classification.unsupported),
        format_count(classification.hash_suppressed),
        format_count(classification.unclassified),
        format_count(classification.failed)
    )?;
    for failure in &observations.failed_by_operation_and_category {
        writeln!(
            output,
            "Observed failure: {} / {} = {}",
            operation_label(failure.operation),
            failure.error_category,
            format_count(failure.failed_requests)
        )?;
    }
    writeln!(output)?;

    let rows = accounting
        .by_operation
        .iter()
        .map(display_row)
        .collect::<Vec<_>>();
    let operation_width = column_width("Operation", rows.iter().map(|row| row.operation));
    let requests_width = column_width("Requests", rows.iter().map(|row| row.requests.as_str()));
    let baseline_requests_width = column_width(
        "Compared",
        rows.iter().map(|row| row.baseline_requests.as_str()),
    );
    let baseline_width = column_width("Baseline", rows.iter().map(|row| row.baseline.as_str()));
    let source_width = column_width("Source", rows.iter().map(|row| row.source.as_str()));
    let metadata_width = column_width("Metadata", rows.iter().map(|row| row.metadata.as_str()));
    let protocol_width = column_width("Protocol", rows.iter().map(|row| row.protocol.as_str()));
    let total_width = column_width("Total", rows.iter().map(|row| row.total.as_str()));
    let net_width = column_width("Delta", rows.iter().map(|row| row.net.as_str()));

    let header = format!(
        "{:<operation_width$}  {:>requests_width$}  {:>baseline_requests_width$}  {:>baseline_width$}  {:>source_width$}  {:>metadata_width$}  {:>protocol_width$}  {:>total_width$}  {:>net_width$}",
        "Operation",
        "Requests",
        "Compared",
        "Baseline",
        "Source",
        "Metadata",
        "Protocol",
        "Total",
        "Delta"
    );
    let rule = format!(
        "{}  {}  {}  {}  {}  {}  {}  {}  {}",
        "-".repeat(operation_width),
        "-".repeat(requests_width),
        "-".repeat(baseline_requests_width),
        "-".repeat(baseline_width),
        "-".repeat(source_width),
        "-".repeat(metadata_width),
        "-".repeat(protocol_width),
        "-".repeat(total_width),
        "-".repeat(net_width)
    );
    writeln!(output, "{}", palette.paint(CYAN, &header))?;
    writeln!(output, "{}", palette.paint(DIM, &rule))?;

    for row in rows {
        let operation = format!("{:<operation_width$}", row.operation);
        let requests = format!("{:>requests_width$}", row.requests);
        let baseline_requests = format!("{:>baseline_requests_width$}", row.baseline_requests);
        let baseline = format!("{:>baseline_width$}", row.baseline);
        let source = format!("{:>source_width$}", row.source);
        let metadata = format!("{:>metadata_width$}", row.metadata);
        let protocol = format!("{:>protocol_width$}", row.protocol);
        let total = format!("{:>total_width$}", row.total);
        let net = format!("{:>net_width$}", row.net);
        let metric_style = match row.net_tokens.cmp(&0) {
            std::cmp::Ordering::Greater => GREEN,
            std::cmp::Ordering::Less => YELLOW,
            std::cmp::Ordering::Equal => DIM,
        };
        writeln!(
            output,
            "{}  {requests}  {baseline_requests}  {baseline}  {source}  {metadata}  {protocol}  {total}  {}",
            palette.paint(CYAN, &operation),
            palette.paint(metric_style, &net)
        )?;
    }

    writeln!(output)?;
    writeln!(
        output,
        "{}",
        palette.paint(
            DIM,
            &format!("Response-delta basis: {}", accounting.estimate_basis)
        )
    )?;
    writeln!(
        output,
        "{}",
        palette.paint(DIM, &format!("Scope: {}", accounting.accounting_scope))
    )?;
    writeln!(
        output,
        "{}",
        palette.paint(
            DIM,
            &format!("Observation scope: {}", observations.observation_scope)
        )
    )?;
    writeln!(
        output,
        "{}",
        palette.paint(
            DIM,
            &format!(
                "Unobserved task outcomes: {}",
                observations.unobserved.join("; ")
            )
        )
    )?;
    Ok(())
}

fn display_row(row: &ResponseTokenAccountingByOperation) -> DisplayRow {
    DisplayRow {
        operation: operation_label(row.operation),
        requests: format_count(row.tracked_requests),
        baseline_requests: format_count(row.baseline_requests),
        baseline: format_count(row.baseline_source_tokens),
        source: format_count(row.response_source_tokens),
        metadata: format_count(row.path_and_metadata_tokens),
        protocol: format_count(row.protocol_tokens),
        total: format_count(row.total_response_tokens),
        net: format_signed_count(row.estimated_net_tokens_saved),
        net_tokens: row.estimated_net_tokens_saved,
    }
}

fn operation_label(operation: TokenAccountingOperation) -> &'static str {
    match operation {
        TokenAccountingOperation::Files => "Files",
        TokenAccountingOperation::Search => "Search",
        TokenAccountingOperation::Outline => "Outline",
        TokenAccountingOperation::Read => "Read",
        TokenAccountingOperation::ContextPlan => "Context plan",
        TokenAccountingOperation::Context => "Context",
        TokenAccountingOperation::Json => "JSON",
        TokenAccountingOperation::History => "History",
        TokenAccountingOperation::ReceiptRebase => "Receipt rebase",
    }
}

fn column_width<'a>(header: &str, values: impl Iterator<Item = &'a str>) -> usize {
    values
        .map(str::len)
        .fold(header.len(), |width, value| width.max(value))
}

fn format_count(value: u64) -> String {
    let digits = value.to_string();
    let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            formatted.push(',');
        }
        formatted.push(digit);
    }
    formatted
}

fn format_signed_count(value: i64) -> String {
    if value < 0 {
        format!("-{}", format_count(value.unsigned_abs()))
    } else {
        format_count(value as u64)
    }
}

fn format_net_reduction(saved: i64, baseline: u64) -> String {
    if baseline == 0 {
        return "--".into();
    }
    let negative = saved < 0;
    let tenths = (u128::from(saved.unsigned_abs()) * 1_000 + u128::from(baseline) / 2)
        / u128::from(baseline);
    format!(
        "{}{}.{:01}%",
        if negative { "-" } else { "" },
        tenths / 10,
        tenths % 10
    )
}

fn color_enabled(is_terminal: bool) -> bool {
    if env::var_os("NO_COLOR").is_some()
        || env::var_os("CLICOLOR").is_some_and(|value| value == "0")
    {
        return false;
    }
    if env::var_os("CLICOLOR_FORCE").is_some_and(|value| value != "0") {
        return true;
    }
    is_terminal && env::var_os("TERM").is_none_or(|value| value != "dumb")
}

#[cfg(test)]
mod tests {
    use super::*;
    use leantoken::{
        ResponseTokenAccounting, ServiceFailureObservation, TokenSavingsObservations,
        TokenSavingsReport, TokenSavingsRequestClassification,
    };

    fn report() -> ObservedTokenSavingsReport {
        ObservedTokenSavingsReport {
            report: TokenSavingsReport {
                response_accounting: ResponseTokenAccounting {
                    accounting_scope: "successful responses; excludes pre-response failures".into(),
                    estimate_basis:
                        "represented-source baseline minus complete serialized response tokens"
                            .into(),
                    tracked_requests: 27,
                    baseline_requests: 24,
                    baseline_source_tokens: 324_656,
                    response_source_tokens: 9_263,
                    path_and_metadata_tokens: 12_000,
                    protocol_tokens: 2_400,
                    total_response_tokens: 23_663,
                    estimated_net_tokens_saved: 300_993,
                    receipt_suppressed_exact: 2,
                    receipt_suppressed_overlap: 1,
                    by_operation: vec![
                        ResponseTokenAccountingByOperation {
                            operation: TokenAccountingOperation::Search,
                            tracked_requests: 9,
                            baseline_requests: 9,
                            baseline_source_tokens: 224_396,
                            response_source_tokens: 3_513,
                            path_and_metadata_tokens: 2_000,
                            protocol_tokens: 487,
                            total_response_tokens: 6_000,
                            estimated_net_tokens_saved: 218_396,
                            receipt_suppressed_exact: 1,
                            receipt_suppressed_overlap: 0,
                        },
                        ResponseTokenAccountingByOperation {
                            operation: TokenAccountingOperation::Files,
                            tracked_requests: 1,
                            baseline_requests: 0,
                            baseline_source_tokens: 0,
                            response_source_tokens: 0,
                            path_and_metadata_tokens: 400,
                            protocol_tokens: 100,
                            total_response_tokens: 500,
                            estimated_net_tokens_saved: -500,
                            receipt_suppressed_exact: 0,
                            receipt_suppressed_overlap: 0,
                        },
                    ],
                },
            },
            observations: TokenSavingsObservations {
                observation_scope:
                    "best-effort service records; local writer contention skips accounting".into(),
                successful_response_records: 27,
                responses_with_baseline: 24,
                failed_service_requests: 3,
                expected_hash_not_modified_responses: 2,
                expected_hash_suppressed_source_tokens: 8_192,
                request_classification: TokenSavingsRequestClassification {
                    useful: 20,
                    incomplete: 3,
                    unsupported: 1,
                    hash_suppressed: 2,
                    unclassified: 0,
                    failed: 3,
                },
                failed_by_operation_and_category: vec![ServiceFailureObservation {
                    operation: TokenAccountingOperation::Search,
                    error_category: "invalid_input".into(),
                    failed_requests: 3,
                }],
                unobserved: vec![
                    "retry chains without a host task/outcome identifier".into(),
                    "unused or irrelevant evidence".into(),
                ],
            },
        }
    }

    #[test]
    fn human_report_formats_summary_table_and_scope() {
        let mut output = Vec::new();
        write_human_report(&mut output, &report(), false).expect("human report");
        let output = String::from_utf8(output).expect("UTF-8 report");

        assert!(output.starts_with(
            "LeanToken Observed Token Accounting\n===================================\n"
        ));
        assert!(output.contains(
            "300,993 fewer response tokens than represented source  (92.7% response delta)"
        ));
        assert!(output.contains("324,656 baseline  ->  23,663 total response"));
        assert!(output.contains("9,263 source + 12,000 metadata + 2,400 protocol"));
        assert!(output.contains("27 successful responses  |  24 with baselines"));
        assert!(output.contains(
            "Persisted observations: 27 successful  |  3 failed  |  2 expected-hash not-modified"
        ));
        assert!(output.contains("Expected-hash suppression: 8,192 represented-source tokens"));
        assert!(output.contains("Observed failure: Search / invalid_input = 3"));
        assert!(output.contains("Operation  Requests  Compared"));
        assert!(output.contains("Search"));
        assert!(output.contains("218,396"));
        assert!(output.contains("Files"));
        assert!(output.contains("-500"));
        assert!(output.contains("Scope: successful responses; excludes pre-response failures"));
        assert!(output.contains("Observation scope: best-effort service records"));
        assert!(output.contains("Unobserved task outcomes: retry chains"));
        assert!(!output.contains("\x1b["));
    }

    #[test]
    fn human_report_adds_color_without_changing_visible_content() {
        let mut plain = Vec::new();
        write_human_report(&mut plain, &report(), false).expect("plain report");
        let mut colored = Vec::new();
        write_human_report(&mut colored, &report(), true).expect("colored report");
        let colored = String::from_utf8(colored).expect("UTF-8 colored report");

        assert!(colored.contains(BOLD_CYAN));
        assert!(colored.contains(BOLD_GREEN));
        assert!(colored.contains(RESET));
        let without_color = [BOLD_CYAN, BOLD_GREEN, CYAN, GREEN, YELLOW, DIM, RESET]
            .into_iter()
            .fold(colored, |text, code| text.replace(code, ""));
        assert_eq!(without_color.as_bytes(), plain);
    }

    #[test]
    fn count_and_reduction_formatting_cover_zero_and_large_values() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(u64::MAX), "18,446,744,073,709,551,615");
        assert_eq!(format_signed_count(-1_234), "-1,234");
        assert_eq!(format_net_reduction(-1, 3), "-33.3%");
    }
}
