use super::*;

pub(super) const MAX_PARSER_COVERAGE_GROUPS: usize = 20;
const MAX_SAFE_EXTENSION_BYTES: usize = 16;

impl Services {
    /// Return aggregate parser coverage from one pinned index snapshot.
    pub async fn parser_coverage(&self) -> Result<ParserCoverageReport> {
        let this = self.clone();
        self.runtime
            .blocking_executor
            .run(CancellationToken::new(), move |_| {
                this.consistent_allow_empty(|session| {
                    let generation = session.generation();
                    Ok(ParserCoverageReport {
                        repository_generation: generation,
                        coverage: parser_coverage_summary(
                            session.parser_coverage_rows(safe_extension_family)?,
                        ),
                    })
                })
            })
            .await
    }
}

pub(super) fn parser_coverage_summary(rows: ParserCoverageRows) -> ParserCoverageSummary {
    let mut by_language = BTreeMap::<String, ParserLanguageCoverage>::new();
    let mut recognized = ParserCoverageCount::default();
    let mut complete = ParserCoverageCount::default();
    let mut incomplete = ParserCoverageCount::default();
    for row in rows.languages {
        let count = ParserCoverageCount {
            files: row.files,
            source_bytes: row.source_bytes,
        };
        add_coverage_count(&mut recognized, count);
        let language =
            by_language
                .entry(row.language.clone())
                .or_insert_with(|| ParserLanguageCoverage {
                    language: row.language,
                    ..ParserLanguageCoverage::default()
                });
        add_coverage_count(&mut language.total, count);
        if row.structurally_complete {
            add_coverage_count(&mut complete, count);
            add_coverage_count(&mut language.complete, count);
        } else {
            add_coverage_count(&mut incomplete, count);
            add_coverage_count(&mut language.incomplete, count);
        }
    }

    let mut languages = by_language.into_values().collect::<Vec<_>>();
    languages.sort_by(|left, right| {
        right
            .total
            .files
            .cmp(&left.total.files)
            .then_with(|| left.language.cmp(&right.language))
    });
    let other_languages = sum_language_remainder(&languages, MAX_PARSER_COVERAGE_GROUPS);
    languages.truncate(MAX_PARSER_COVERAGE_GROUPS);

    let mut unrecognized = ParserCoverageCount::default();
    let mut by_extension = BTreeMap::<String, ParserCoverageCount>::new();
    for extension in rows.unrecognized_extensions {
        let count = ParserCoverageCount {
            files: extension.files,
            source_bytes: extension.source_bytes,
        };
        add_coverage_count(&mut unrecognized, count);
        add_coverage_count(by_extension.entry(extension.extension).or_default(), count);
    }
    let mut unrecognized_extensions = by_extension
        .into_iter()
        .map(|(extension, total)| ParserExtensionCoverage { extension, total })
        .collect::<Vec<_>>();
    unrecognized_extensions.sort_by(|left, right| {
        right
            .total
            .files
            .cmp(&left.total.files)
            .then_with(|| left.extension.cmp(&right.extension))
    });
    let other_unrecognized_extensions = unrecognized_extensions
        .iter()
        .skip(MAX_PARSER_COVERAGE_GROUPS)
        .fold(ParserCoverageCount::default(), |mut total, group| {
            add_coverage_count(&mut total, group.total);
            total
        });
    unrecognized_extensions.truncate(MAX_PARSER_COVERAGE_GROUPS);

    let mut indexed = recognized;
    add_coverage_count(&mut indexed, unrecognized);
    ParserCoverageSummary {
        indexed,
        recognized,
        complete,
        incomplete,
        unrecognized,
        languages,
        other_languages,
        unrecognized_extensions,
        other_unrecognized_extensions,
    }
}

fn add_coverage_count(total: &mut ParserCoverageCount, value: ParserCoverageCount) {
    total.files = total.files.saturating_add(value.files);
    total.source_bytes = total.source_bytes.saturating_add(value.source_bytes);
}

fn sum_language_remainder(
    languages: &[ParserLanguageCoverage],
    retained: usize,
) -> ParserCoverageCount {
    languages
        .iter()
        .skip(retained)
        .fold(ParserCoverageCount::default(), |mut total, language| {
            add_coverage_count(&mut total, language.total);
            total
        })
}

pub(super) fn safe_extension_family(path: &str) -> String {
    let Some(extension) = std::path::Path::new(path)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
    else {
        return "[no_extension]".to_owned();
    };
    if extension.is_empty()
        || extension.len() > MAX_SAFE_EXTENSION_BYTES
        || !extension
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'_'))
    {
        return "[other_extension]".to_owned();
    }
    format!(".{}", extension.to_ascii_lowercase())
}
