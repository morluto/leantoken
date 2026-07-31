pub(super) fn parse_revision_range(revision: &str) -> Result<Option<(&str, &str)>> {
    let Some((base, head)) = revision.split_once("..") else {
        return Ok(None);
    };
    if base.trim().is_empty()
        || head.trim().is_empty()
        || base.ends_with('.')
        || head.starts_with('.')
        || head.contains("..")
    {
        return Err(Error::InvalidInput {
            field: "base revision",
            reason: "revision range must be BASE..HEAD",
        });
    }
    Ok(Some((base, head)))
}

#[derive(Clone, Copy)]
pub(super) enum ContextExcerptKind {
    Symbol,
    Reference,
    Text,
    ImportSymbol,
}

impl ContextExcerptKind {
    pub(super) const fn token_cap(self) -> usize {
        match self {
            Self::Symbol => SYMBOL_CONTEXT_TOKEN_CAP,
            Self::Reference => REFERENCE_CONTEXT_TOKEN_CAP,
            Self::Text => TEXT_CONTEXT_TOKEN_CAP,
            Self::ImportSymbol => IMPORT_SYMBOL_CONTEXT_TOKEN_CAP,
        }
    }
}

pub(super) fn excerpt_budget(request_budget: usize, kind: ContextExcerptKind) -> usize {
    request_budget.min(kind.token_cap())
}

pub(super) fn context_path_score(path: &str, terms: &[String], task: &str) -> f64 {
    ContextPathScorer::new(terms, task).score(path)
}

pub(super) struct ContextPathScorer {
    pub(super) terms: Vec<String>,
    pub(super) code_token_parts: Vec<Vec<String>>,
    pub(super) languages: [bool; 5],
    pub(super) mcp_repository_intent: bool,
}

impl ContextPathScorer {
    pub(super) fn new(terms: &[String], task: &str) -> Self {
        let code_token_parts = facets::legacy_code_tokens(task)
            .into_iter()
            .filter(|token| {
                token.contains("::")
                    || token
                        .split('.')
                        .any(|part| part.chars().next().is_some_and(char::is_uppercase))
            })
            .map(|code_token| {
                expand_terms(&code_token)
                    .into_iter()
                    .map(|part| part.to_ascii_lowercase())
                    .filter(|part| part.chars().count() >= 2)
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .collect()
            })
            .collect();
        Self {
            terms: terms.iter().map(|term| term.to_ascii_lowercase()).collect(),
            code_token_parts,
            languages: [
                task_mentions_language(task, "javascript"),
                task_mentions_language(task, "typescript"),
                task_mentions_language(task, "python"),
                task_mentions_language(task, "rust"),
                task_mentions_language(task, "go"),
            ],
            mcp_repository_intent: mcp_repository_intent(terms, task),
        }
    }

    pub(super) fn score(&self, path: &str) -> f64 {
        let path = path.to_lowercase();
        let mut score = f64::from(
            u32::try_from(
                self.terms
                    .iter()
                    .filter(|term| path.contains(term.as_str()))
                    .count(),
            )
            .unwrap_or(u32::MAX),
        );
        for parts in &self.code_token_parts {
            let matched_parts = parts.iter().filter(|part| path.contains(*part)).count();
            if matched_parts >= 2 {
                let matched_parts = u32::try_from(matched_parts).unwrap_or(u32::MAX);
                score += f64::from(matched_parts.saturating_mul(matched_parts));
            }
        }
        for (mentioned, component, extensions) in [
            (self.languages[0], "/js/", &["js", "jsx", "mjs", "cjs"][..]),
            (self.languages[1], "/ts/", &["ts", "tsx"][..]),
            (self.languages[2], "/python/", &["py", "pyw"][..]),
            (self.languages[3], "/rust/", &["rs"][..]),
            (self.languages[4], "/go/", &["go"][..]),
        ] {
            let extension_matches = path
                .rsplit_once('.')
                .is_some_and(|(_, extension)| extensions.contains(&extension));
            let component = component.trim_matches('/');
            let component_matches = path
                .split('/')
                .any(|path_component| path_component == component);
            if mentioned && (extension_matches || component_matches) {
                // An explicit language name in the task is strong repository-scope
                // evidence. Keep this above an exact-name match in another
                // language so common names such as `Point` do not dominate.
                score += 12.0;
            }
        }
        if self.mcp_repository_intent {
            if path == "src/mcp.rs"
                || path.starts_with("src/mcp/")
                || path == "src/main/mcp_runtime.rs"
            {
                score += 14.0;
            } else if path == "crates/test-suite/src/domains/protocol.rs"
                || path == "crates/test-suite/src/domains/contracts.rs"
                || path == "tests/mcp.rs"
                || path.starts_with("tests/mcp/")
            {
                score += 8.0;
            }
        }
        score
    }
}

pub(super) fn mcp_repository_intent(terms: &[String], task: &str) -> bool {
    let terms = terms
        .iter()
        .map(|term| term.to_ascii_lowercase())
        .chain(
            task.split(|character: char| !character.is_alphanumeric() && character != '_')
                .filter(|term| !term.is_empty())
                .map(str::to_ascii_lowercase),
        )
        .collect::<HashSet<_>>();
    if terms.contains("mcp") {
        return true;
    }
    let surface_terms = ["tool", "tools", "catalog", "schema", "schemas"];
    let registration_terms = ["registration", "registered", "register", "server"];
    surface_terms.iter().any(|term| terms.contains(*term))
        && registration_terms.iter().any(|term| terms.contains(*term))
}

pub(super) fn context_path_group(path: &str) -> String {
    let mut components = path.split('/').filter(|component| !component.is_empty());
    let first = components.next().unwrap_or("<root>");
    let second = components.next();
    if second.is_none() {
        return "<root>".into();
    }
    if matches!(
        first,
        "src" | "lib" | "app" | "apps" | "crates" | "packages"
    ) && let Some(second) = second
    {
        format!("{first}/{second}")
    } else {
        first.to_owned()
    }
}

pub(super) fn build_context_routing(
    request: &ContextRequest,
    scope: &DiffScopeReceipt,
    candidate_paths: usize,
    selected_paths: &[String],
) -> Option<ContextRoutingReceipt> {
    if scope.changed_paths.len() < OVERSIZED_CHANGE_PATHS {
        return None;
    }
    let mut grouped_paths = BTreeMap::<String, Vec<String>>::new();
    for path in &scope.changed_paths {
        grouped_paths
            .entry(context_path_group(path))
            .or_default()
            .push(path.clone());
    }
    if grouped_paths.len() < MIN_OVERSIZED_PATH_GROUPS {
        return None;
    }
    let path_groups_total = grouped_paths.len();
    let mut groups = grouped_paths.into_iter().collect::<Vec<_>>();
    groups.sort_by(|left, right| {
        right
            .1
            .len()
            .cmp(&left.1.len())
            .then_with(|| left.0.cmp(&right.0))
    });
    let selected_groups = selected_paths.iter().fold(
        BTreeMap::<String, BTreeSet<&str>>::new(),
        |mut paths, path| {
            paths
                .entry(context_path_group(path))
                .or_default()
                .insert(path);
            paths
        },
    );
    let selected_path_count = selected_paths
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        .len();
    let strongest_selected_group = selected_groups
        .values()
        .map(BTreeSet::len)
        .max()
        .unwrap_or(0);
    let weakly_concentrated = selected_path_count > 1
        && strongest_selected_group.saturating_mul(2) <= selected_path_count;

    let suggestions = groups
        .iter()
        .take(MAX_ROUTING_SUGGESTIONS)
        .map(|(prefix, _)| ContextRoutingSuggestion {
            include_paths: vec![if prefix == "<root>" {
                "*".into()
            } else {
                format!("{prefix}/**")
            }],
        })
        .collect();
    let path_groups = groups
        .into_iter()
        .take(MAX_ROUTING_GROUPS)
        .map(|(prefix, paths)| ContextPathGroup {
            prefix,
            changed_paths: paths.len(),
        })
        .collect();
    Some(ContextRoutingReceipt {
        candidate_paths,
        changed_paths: scope.changed_paths.len(),
        selected_paths: selected_path_count,
        weakly_concentrated,
        consistency: IndexConsistency::IndexedGeneration,
        base_revision: request.base_revision.clone(),
        known_hashes: request.known_hashes.clone(),
        path_groups_total,
        path_groups,
        suggestions,
    })
}

pub(super) fn resolve_context_workflow(requested: ContextWorkflow, task: &str) -> ContextWorkflow {
    if requested != ContextWorkflow::Auto {
        return requested;
    }
    let task = task.to_ascii_lowercase();
    if ["pull request", "contribution", "contributing"]
        .iter()
        .any(|signal| task.contains(signal))
    {
        ContextWorkflow::Contribution
    } else if ["code review", "review this", "review the changes"]
        .iter()
        .any(|signal| task.contains(signal))
    {
        ContextWorkflow::Review
    } else if ["investigate", "root cause", "diagnose"]
        .iter()
        .any(|signal| task.contains(signal))
    {
        ContextWorkflow::Investigation
    } else {
        ContextWorkflow::Implementation
    }
}

pub(super) fn workflow_path_role(
    path: &str,
    request: &ContextRequest,
) -> Option<(f64, &'static str)> {
    let normalized = path.replace('\\', "/");
    let lower = normalized.to_ascii_lowercase();
    let mut best = WORKFLOW_PATHS
        .matches(&normalized)
        .into_iter()
        .map(|index| WORKFLOW_PATH_ROLES[index])
        .max_by(|left, right| left.0.total_cmp(&right.0));
    if likely_owner_test(&lower, request) && best.is_none_or(|(score, _)| score < OWNER_TEST_SCORE)
    {
        best = Some((OWNER_TEST_SCORE, "owner_test"));
    }
    best
}

pub(super) const OWNER_TEST_SCORE: f64 = 3.75;
pub(super) const WORKFLOW_PATH_RULES: [(&str, f64, &str); 15] = [
    ("AGENTS.md", 5.0, "guidance"),
    ("**/AGENTS.md", 5.0, "guidance"),
    ("CONTRIBUTING*", 5.0, "guidance"),
    ("**/CONTRIBUTING*", 5.0, "guidance"),
    (".github/PULL_REQUEST_TEMPLATE*", 5.0, "guidance"),
    (".github/PULL_REQUEST_TEMPLATE/**", 5.0, "guidance"),
    (".github/ISSUE_TEMPLATE/**", 4.5, "template"),
    ("docs/development*", 4.25, "guidance"),
    ("docs/contributing*", 4.25, "guidance"),
    ("docs/testing*", 4.25, "guidance"),
    (".github/workflows/**", 3.0, "validation"),
    ("Cargo.toml", 3.0, "validation"),
    ("Makefile", 3.0, "validation"),
    ("justfile", 3.0, "validation"),
    ("{package.json,pyproject.toml,go.mod}", 3.0, "validation"),
];
pub(super) const WORKFLOW_PATH_ROLES: [(f64, &str); WORKFLOW_PATH_RULES.len()] =
    workflow_path_roles();

pub(super) const fn workflow_path_roles() -> [(f64, &'static str); WORKFLOW_PATH_RULES.len()] {
    let mut roles = [(0.0, ""); WORKFLOW_PATH_RULES.len()];
    let mut index = 0;
    while index < WORKFLOW_PATH_RULES.len() {
        roles[index] = (WORKFLOW_PATH_RULES[index].1, WORKFLOW_PATH_RULES[index].2);
        index += 1;
    }
    roles
}

static WORKFLOW_PATHS: LazyLock<GlobSet> = LazyLock::new(|| {
    let mut builder = GlobSetBuilder::new();
    for (pattern, _, _) in WORKFLOW_PATH_RULES {
        builder.add(
            GlobBuilder::new(pattern)
                .case_insensitive(true)
                .literal_separator(true)
                .build()
                .expect("static workflow glob"),
        );
    }
    builder.build().expect("static workflow glob set")
});

pub(super) fn likely_owner_test(path: &str, request: &ContextRequest) -> bool {
    owner_test_changed_path(path, request).is_some()
}

pub(super) fn owner_test_changed_path(path: &str, request: &ContextRequest) -> Option<String> {
    let path = path.replace('\\', "/").to_ascii_lowercase();
    let is_test = path.contains("/test")
        || path.starts_with("test")
        || path.contains("/spec")
        || path.starts_with("spec");
    if !is_test {
        return None;
    }
    let test_name = path.rsplit('/').next().unwrap_or(&path);
    let test_stem = test_name.split('.').next().unwrap_or(test_name);
    request
        .changed_paths
        .iter()
        .chain(&request.focus_paths)
        .find_map(|changed| {
            let normalized = changed.replace('\\', "/");
            let stem = normalized
                .rsplit('/')
                .next()
                .and_then(|name| name.split('.').next())?
                .to_ascii_lowercase();
            (stem.len() >= 3 && contains_filename_token(test_stem, &stem)).then(|| changed.clone())
        })
}

pub(super) fn contains_filename_token(path: &str, token: &str) -> bool {
    path.match_indices(token).any(|(start, matched)| {
        let end = start + matched.len();
        let boundary =
            |character: Option<char>| character.is_none_or(|value| !value.is_alphanumeric());
        boundary(path[..start].chars().next_back()) && boundary(path[end..].chars().next())
    })
}
use super::*;
