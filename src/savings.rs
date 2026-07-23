use std::{
    env,
    io::{IsTerminal, Write},
};

use leantoken::{Result, TokenSavingsByOperation, TokenSavingsOperation, TokenSavingsResponse};

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
    baseline: String,
    emitted: String,
    saved: String,
    reduction: String,
    has_savings: bool,
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

pub(crate) fn print_report(report: &TokenSavingsResponse, json_output: bool) -> Result<()> {
    let stdout = std::io::stdout();
    let color = color_enabled(stdout.is_terminal());
    let mut output = stdout.lock();
    if json_output {
        serde_json::to_writer(&mut output, report)?;
        output.write_all(b"\n")?;
        return Ok(());
    }
    write_human_report(&mut output, report, color)
}

fn write_human_report(
    output: &mut impl Write,
    report: &TokenSavingsResponse,
    color: bool,
) -> Result<()> {
    let palette = Palette { enabled: color };
    let total_reduction = format_reduction(
        report.estimated_source_tokens_saved,
        report.baseline_source_tokens,
    );
    let saved = format_count(report.estimated_source_tokens_saved);
    let saved_summary = if report.estimated_source_tokens_saved == 0 {
        palette.paint(DIM, &saved)
    } else {
        palette.paint(BOLD_GREEN, &saved)
    };
    let reduction_summary = match total_reduction.as_str() {
        "--" => palette.paint(DIM, "--"),
        reduction => palette.paint(BOLD_GREEN, reduction),
    };
    let count_quality = if report.token_count_exact {
        palette.paint(GREEN, "exact token count")
    } else {
        palette.paint(YELLOW, "estimated token count")
    };

    writeln!(output, "{}", palette.paint(BOLD_CYAN, "LeanToken Savings"))?;
    writeln!(output, "{}", palette.paint(DIM, "================="))?;
    writeln!(
        output,
        "{saved_summary} source tokens saved  ({reduction_summary} reduction)"
    )?;
    writeln!(
        output,
        "{} baseline  ->  {} emitted",
        format_count(report.baseline_source_tokens),
        format_count(report.emitted_source_tokens)
    )?;
    writeln!(
        output,
        "{} tracked requests  |  {} ({count_quality})",
        format_count(report.tracked_requests),
        palette.paint(CYAN, &report.tokenizer)
    )?;
    writeln!(output)?;

    let rows = report
        .by_operation
        .iter()
        .map(display_row)
        .collect::<Vec<_>>();
    let operation_width = column_width("Operation", rows.iter().map(|row| row.operation));
    let requests_width = column_width("Requests", rows.iter().map(|row| row.requests.as_str()));
    let baseline_width = column_width("Baseline", rows.iter().map(|row| row.baseline.as_str()));
    let emitted_width = column_width("Emitted", rows.iter().map(|row| row.emitted.as_str()));
    let saved_width = column_width("Saved", rows.iter().map(|row| row.saved.as_str()));
    let reduction_width = column_width("Reduction", rows.iter().map(|row| row.reduction.as_str()));

    let header = format!(
        "{:<operation_width$}  {:>requests_width$}  {:>baseline_width$}  {:>emitted_width$}  {:>saved_width$}  {:>reduction_width$}",
        "Operation", "Requests", "Baseline", "Emitted", "Saved", "Reduction"
    );
    let rule = format!(
        "{}  {}  {}  {}  {}  {}",
        "-".repeat(operation_width),
        "-".repeat(requests_width),
        "-".repeat(baseline_width),
        "-".repeat(emitted_width),
        "-".repeat(saved_width),
        "-".repeat(reduction_width)
    );
    writeln!(output, "{}", palette.paint(CYAN, &header))?;
    writeln!(output, "{}", palette.paint(DIM, &rule))?;

    for row in rows {
        let operation = format!("{:<operation_width$}", row.operation);
        let requests = format!("{:>requests_width$}", row.requests);
        let baseline = format!("{:>baseline_width$}", row.baseline);
        let emitted = format!("{:>emitted_width$}", row.emitted);
        let saved = format!("{:>saved_width$}", row.saved);
        let reduction = format!("{:>reduction_width$}", row.reduction);
        let metric_style = if row.has_savings { GREEN } else { DIM };
        writeln!(
            output,
            "{}  {requests}  {baseline}  {emitted}  {}  {}",
            palette.paint(CYAN, &operation),
            palette.paint(metric_style, &saved),
            palette.paint(metric_style, &reduction)
        )?;
    }

    writeln!(output)?;
    writeln!(
        output,
        "{}",
        palette.paint(DIM, &format!("Basis: {}", report.estimate_basis))
    )?;
    writeln!(
        output,
        "{}",
        palette.paint(
            DIM,
            "Source only; excludes protocol overhead, billing, caching, and evidence quality."
        )
    )?;
    Ok(())
}

fn display_row(row: &TokenSavingsByOperation) -> DisplayRow {
    DisplayRow {
        operation: operation_label(row.operation),
        requests: format_count(row.tracked_requests),
        baseline: format_count(row.baseline_source_tokens),
        emitted: format_count(row.emitted_source_tokens),
        saved: format_count(row.estimated_source_tokens_saved),
        reduction: format_reduction(
            row.estimated_source_tokens_saved,
            row.baseline_source_tokens,
        ),
        has_savings: row.estimated_source_tokens_saved > 0,
    }
}

fn operation_label(operation: TokenSavingsOperation) -> &'static str {
    match operation {
        TokenSavingsOperation::Search => "Search",
        TokenSavingsOperation::Outline => "Outline",
        TokenSavingsOperation::Read => "Read",
        TokenSavingsOperation::Context => "Context",
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

fn format_reduction(saved: u64, baseline: u64) -> String {
    if baseline == 0 {
        return "--".into();
    }
    let tenths = (u128::from(saved) * 1_000 + u128::from(baseline) / 2) / u128::from(baseline);
    format!("{}.{:01}%", tenths / 10, tenths % 10)
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

    fn report() -> TokenSavingsResponse {
        TokenSavingsResponse {
            tokenizer: "o200k_base".into(),
            token_count_exact: true,
            estimate_basis:
                "requested read ranges or whole source files represented in each response".into(),
            tracked_requests: 24,
            baseline_source_tokens: 324_656,
            emitted_source_tokens: 9_263,
            estimated_source_tokens_saved: 315_393,
            by_operation: vec![
                TokenSavingsByOperation {
                    operation: TokenSavingsOperation::Search,
                    tracked_requests: 9,
                    baseline_source_tokens: 224_396,
                    emitted_source_tokens: 3_513,
                    estimated_source_tokens_saved: 220_883,
                },
                TokenSavingsByOperation {
                    operation: TokenSavingsOperation::Read,
                    tracked_requests: 13,
                    baseline_source_tokens: 4_198,
                    emitted_source_tokens: 4_198,
                    estimated_source_tokens_saved: 0,
                },
            ],
        }
    }

    #[test]
    fn human_report_formats_summary_table_and_scope() {
        let mut output = Vec::new();
        write_human_report(&mut output, &report(), false).expect("human report");
        let output = String::from_utf8(output).expect("UTF-8 report");

        assert!(output.starts_with("LeanToken Savings\n=================\n"));
        assert!(output.contains("315,393 source tokens saved  (97.1% reduction)"));
        assert!(output.contains("324,656 baseline  ->  9,263 emitted"));
        assert!(output.contains("24 tracked requests  |  o200k_base (exact token count)"));
        assert!(output.contains("Operation  Requests  Baseline  Emitted    Saved  Reduction"));
        assert!(output.contains("Search            9   224,396    3,513  220,883      98.4%"));
        assert!(output.contains("Read             13     4,198    4,198        0       0.0%"));
        assert!(output.contains("Source only; excludes protocol overhead"));
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
        assert_eq!(format_reduction(0, 0), "--");
        assert_eq!(format_reduction(0, 10), "0.0%");
        assert_eq!(format_reduction(1, 3), "33.3%");
        assert_eq!(format_reduction(2, 3), "66.7%");
    }
}
