use super::*;
pub(in crate::ranking) const FACET_PREFIX: &str = "facet:";
pub(in crate::ranking) const CHANNEL_PREFIX: &str = "channel:";
pub(in crate::ranking) const REQUIRED_EVIDENCE_PREFIX: &str = "required_evidence:";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ContextPathClass {
    Production,
    Test,
    Auxiliary,
    Supporting,
}

pub(crate) fn required_evidence_marker(requirement: usize, query: usize) -> String {
    format!("{REQUIRED_EVIDENCE_PREFIX}{requirement}:{query}")
}

pub(in crate::ranking) fn required_evidence_query(
    candidate: &Candidate,
    requirement: usize,
    query: usize,
) -> bool {
    let marker = required_evidence_marker(requirement, query);
    candidate.match_kinds.iter().any(|kind| kind == &marker)
}

pub(in crate::ranking) fn carries_required_evidence(
    candidate: &Candidate,
    requirement: usize,
) -> bool {
    let prefix = format!("{REQUIRED_EVIDENCE_PREFIX}{requirement}:");
    candidate
        .match_kinds
        .iter()
        .any(|kind| kind.starts_with(&prefix))
}

pub(in crate::ranking) fn carries_facet(candidate: &Candidate, facet: &str) -> bool {
    let prefix = format!("{FACET_PREFIX}{facet}:");
    candidate
        .match_kinds
        .iter()
        .any(|kind| kind.starts_with(&prefix))
}

pub(in crate::ranking) fn carries_specific_exact_atom(candidate: &Candidate) -> bool {
    let prefix = format!("{FACET_PREFIX}exact_atom:");
    candidate.match_kinds.iter().any(|kind| {
        let Some(atom) = kind.strip_prefix(&prefix) else {
            return false;
        };
        let prose_hyphen = atom.contains('-')
            && atom
                .chars()
                .all(|character| character.is_ascii_lowercase() || character == '-');
        !prose_hyphen
            && (atom.chars().count() >= 5
                || atom
                    .chars()
                    .any(|character| !character.is_ascii_alphanumeric()))
    })
}

pub(in crate::ranking) fn carries_specific_primary_change(candidate: &Candidate) -> bool {
    let prefix = format!("{FACET_PREFIX}primary_change:");
    candidate.match_kinds.iter().any(|kind| {
        let Some(facet) = kind.strip_prefix(&prefix) else {
            return false;
        };
        facet.chars().count() >= 5
            || facet
                .chars()
                .any(|character| !character.is_ascii_alphanumeric())
    })
}

pub(in crate::ranking) fn facet_value_count(candidate: &Candidate, facet: &str) -> usize {
    let prefix = format!("{FACET_PREFIX}{facet}:");
    candidate
        .match_kinds
        .iter()
        .filter(|kind| kind.starts_with(&prefix))
        .count()
}

pub(in crate::ranking) fn carries_all_facet_values(
    candidate: &Candidate,
    baseline: &Candidate,
    facet: &str,
) -> bool {
    let prefix = format!("{FACET_PREFIX}{facet}:");
    baseline
        .match_kinds
        .iter()
        .filter(|kind| kind.starts_with(&prefix))
        .all(|kind| candidate.match_kinds.contains(kind))
}

fn is_test_path(path: &str) -> bool {
    let file_name = path.rsplit('/').next().unwrap_or(path);
    let stem = file_name
        .split_once('.')
        .map_or(file_name, |(stem, _)| stem);
    let module_stem = file_name
        .rsplit_once('.')
        .map_or(file_name, |(stem, _)| stem);
    path.starts_with("test/")
        || path.starts_with("tests/")
        || path.starts_with("spec/")
        || path.contains("/test/")
        || path.contains("/tests/")
        || path.contains("/spec/")
        || path.contains("/test-suite/")
        || path.starts_with("crates/test-suite/")
        || path
            .split('/')
            .any(|component| component == "__tests__" || component.starts_with("test_"))
        || module_stem.ends_with(".test")
        || module_stem.ends_with(".spec")
        || matches!(stem, "test" | "tests")
        || file_name.ends_with("_spec.rb")
        || file_name.ends_with(".spec.rb")
        || stem.ends_with("_test")
        || stem.ends_with("_tests")
        || stem.starts_with("test_")
}

fn is_class_or_project_test_path(path: &str) -> bool {
    if !matches!(
        crate::parser::language_by_path(path).as_deref(),
        Some("csharp" | "java")
    ) {
        return false;
    }
    let file_name = path.rsplit('/').next().unwrap_or(path);
    let stem = file_name
        .rsplit_once('.')
        .map_or(file_name, |(stem, _)| stem);
    stem.ends_with("Test")
        || stem.ends_with("Tests")
        || path
            .split('/')
            .any(|component| component.to_ascii_lowercase().ends_with(".tests"))
}

fn is_programming_language_path(path: &str) -> bool {
    matches!(
        crate::parser::language_by_path(path).as_deref(),
        Some(
            "c" | "csharp"
                | "cpp"
                | "java"
                | "rust"
                | "python"
                | "php"
                | "ruby"
                | "javascript"
                | "typescript"
                | "tsx"
                | "go"
                | "html"
                | "css"
        )
    )
}

fn is_supporting_code_path(path: &str) -> bool {
    path.starts_with("docs/")
        || path.contains("/docs/")
        || path.starts_with("examples/")
        || path.contains("/examples/")
        || path.starts_with("example/")
        || path.contains("/example/")
        || path.starts_with("benches/")
        || path.contains("/benches/")
        || path.starts_with("benchmarks/")
        || path.contains("/benchmarks/")
        || path.starts_with("scripts/")
        || path.contains("/scripts/")
        || path.starts_with("tools/")
}

fn normalized_source_stem(path: &str) -> String {
    let file_name = path.rsplit('/').next().unwrap_or(path);
    let stem = file_name
        .rsplit_once('.')
        .map_or(file_name, |(stem, _)| stem);
    let stem = stem
        .strip_suffix(".test")
        .or_else(|| stem.strip_suffix(".spec"))
        .unwrap_or(stem);
    let stem = stem
        .strip_suffix("_tests")
        .or_else(|| stem.strip_suffix("_test"))
        .or_else(|| stem.strip_suffix("_spec"))
        .or_else(|| stem.strip_prefix("test_"))
        .unwrap_or(stem);
    stem.to_ascii_lowercase()
}

pub(in crate::ranking) fn owner_test_path_affinity(owner: &str, test: &str) -> usize {
    let owner_parent = owner.rsplit_once('/').map_or("", |(parent, _)| parent);
    let test_parent = test.rsplit_once('/').map_or("", |(parent, _)| parent);
    let same_parent = owner_parent == test_parent;
    let owner_stem = normalized_source_stem(owner);
    let test_stem = normalized_source_stem(test);
    let directory_module_test = same_parent
        && (!owner_parent.is_empty())
        && owner_parent
            .rsplit('/')
            .next()
            .is_some_and(|parent| parent.eq_ignore_ascii_case(&test_stem));
    if (owner_stem == test_stem && same_parent) || directory_module_test {
        4
    } else if owner_stem.chars().count() < 4 || test_stem.chars().count() < 4 {
        0
    } else if owner_stem == test_stem
        || owner_stem.starts_with(&test_stem)
        || test_stem.starts_with(&owner_stem)
    {
        2
    } else if owner_stem
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|part| part.chars().count() >= 4)
        .any(|part| {
            test_stem
                .split(|character: char| !character.is_ascii_alphanumeric())
                .any(|test_part| test_part == part)
        })
    {
        1
    } else {
        0
    }
}

pub(crate) fn context_path_class(path: &str) -> ContextPathClass {
    let class_or_project_test = is_class_or_project_test_path(path);
    let path = path.to_ascii_lowercase();
    let root_markdown = !path.contains('/')
        && (path.ends_with(".md") || path.ends_with(".mdx"))
        && !matches!(path.as_str(), "readme.md" | "agents.md" | "contributing.md");
    if path.starts_with(".agents/")
        || path.starts_with("fixtures/")
        || path.starts_with("benchmarks/reports/")
        || path.contains("/snapshots/")
        || path.ends_with(".snap")
        || root_markdown
        || path.contains("/requests/")
        || path.contains("/schema/")
    {
        ContextPathClass::Auxiliary
    } else if class_or_project_test || is_test_path(&path) {
        ContextPathClass::Test
    } else if is_supporting_code_path(&path) {
        ContextPathClass::Supporting
    } else if is_programming_language_path(&path) {
        ContextPathClass::Production
    } else {
        ContextPathClass::Supporting
    }
}

pub(crate) fn owner_path_prior(path: &str) -> f64 {
    match context_path_class(path) {
        ContextPathClass::Production => 4.0,
        ContextPathClass::Test => 0.0,
        ContextPathClass::Auxiliary => -8.0,
        ContextPathClass::Supporting => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_path_classes_separate_production_tests_and_auxiliary_evidence() {
        assert_eq!(
            context_path_class("src/services/context.rs"),
            ContextPathClass::Production
        );
        assert_eq!(
            context_path_class("lib/response.js"),
            ContextPathClass::Production
        );
        assert_eq!(
            context_path_class("packages/runtime-core/src/renderer.ts"),
            ContextPathClass::Production
        );
        assert_eq!(
            context_path_class("recovery.go"),
            ContextPathClass::Production
        );
        assert_eq!(context_path_class("cJSON.c"), ContextPathClass::Production);
        assert_eq!(
            context_path_class("crates/test-suite/src/domains/retrieval.rs"),
            ContextPathClass::Test
        );
        assert_eq!(
            context_path_class("src/services/context/tests.rs"),
            ContextPathClass::Test
        );
        assert_eq!(
            context_path_class("test/app.render.js"),
            ContextPathClass::Test
        );
        assert_eq!(
            context_path_class("completions_test.go"),
            ContextPathClass::Test
        );
        assert_eq!(
            context_path_class("tests/unit/foo.spec.ts"),
            ContextPathClass::Test
        );
        assert_eq!(
            context_path_class("spec/models/user.rb"),
            ContextPathClass::Test
        );
        assert_eq!(
            context_path_class("packages/core/spec/widget.spec.rb"),
            ContextPathClass::Test
        );
        assert_eq!(
            context_path_class("src/test_support/helpers.rs"),
            ContextPathClass::Test
        );
        assert_eq!(
            context_path_class("packages/compiler-core/__tests__/testUtils.ts"),
            ContextPathClass::Test
        );
        assert_eq!(
            context_path_class("app/models/user_spec.rb"),
            ContextPathClass::Test
        );
        assert_eq!(
            context_path_class("app/models/widget.spec.rb"),
            ContextPathClass::Test
        );
        assert_eq!(
            context_path_class("examples/route-separation/index.js"),
            ContextPathClass::Supporting
        );
        assert_eq!(
            context_path_class("crates/client/examples/connect.rs"),
            ContextPathClass::Supporting
        );
        assert_eq!(
            context_path_class("docs/conf.py"),
            ContextPathClass::Supporting
        );
        assert_eq!(
            context_path_class("src/mcp/snapshots/tools.snap"),
            ContextPathClass::Auxiliary
        );
        assert_eq!(
            context_path_class("retrieval_notes.md"),
            ContextPathClass::Auxiliary
        );
        assert_eq!(
            context_path_class("README.md"),
            ContextPathClass::Supporting
        );
        assert_eq!(
            context_path_class("Cargo.toml"),
            ContextPathClass::Supporting
        );
    }

    #[test]
    fn class_test_conventions_and_nested_product_tools_are_classified_by_role() {
        assert_eq!(
            context_path_class("MyApp.Tests/UserService.cs"),
            ContextPathClass::Test
        );
        assert_eq!(
            context_path_class("src/UserServiceTests.cs"),
            ContextPathClass::Test
        );
        assert_eq!(
            context_path_class("src/main/java/UserServiceTest.java"),
            ContextPathClass::Test
        );
        assert_eq!(
            context_path_class("src/main/java/Contest.java"),
            ContextPathClass::Production
        );
        assert_eq!(
            context_path_class("src/mcp/tools/catalog.rs"),
            ContextPathClass::Production
        );
        assert_eq!(
            context_path_class("tools/release.rs"),
            ContextPathClass::Supporting
        );
    }

    #[test]
    fn module_test_file_extensions_are_classified_as_tests() {
        assert_eq!(
            context_path_class("npm/npm-packaging.test.mjs"),
            ContextPathClass::Test
        );
        assert_eq!(
            context_path_class("packages/core/widget.spec.mts"),
            ContextPathClass::Test
        );
    }

    #[test]
    fn owner_test_affinity_is_layout_independent_and_name_specific() {
        assert_eq!(
            owner_test_path_affinity("recovery.go", "recovery_test.go"),
            4
        );
        assert_eq!(owner_test_path_affinity("app.go", "app_test.go"), 4);
        assert_eq!(owner_test_path_affinity("src/db.rs", "src/db_test.rs"), 4);
        assert_eq!(
            owner_test_path_affinity("app/models/user.rb", "spec/models/user_spec.rb"),
            2
        );
        assert_eq!(
            owner_test_path_affinity(
                "packages/runtime-core/src/renderer.ts",
                "packages/runtime-core/__tests__/rendererOptimizedMode.spec.ts"
            ),
            2
        );
        assert_eq!(
            owner_test_path_affinity("completions.go", "doc/yaml_docs_test.go"),
            0
        );
        assert_eq!(
            owner_test_path_affinity("powershell_completions.go", "completions_test.go"),
            1
        );
        assert_eq!(
            owner_test_path_affinity("render/json.go", "render/render_test.go"),
            4
        );
        assert_eq!(
            owner_test_path_affinity("render/json.go", "binding/json_test.go"),
            2
        );
        assert_eq!(
            owner_test_path_affinity("render/json.go", "render/reader_test.go"),
            0
        );
        assert_eq!(
            owner_test_path_affinity("packages/a/index.ts", "packages/b/index.test.ts"),
            2
        );
    }
}
