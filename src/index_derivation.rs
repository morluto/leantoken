//! Authoritative identity for persisted index derivation semantics.
//!
//! The digest is intentionally derived from the implementation owners that can
//! change persisted rows, plus the exact parser/tokenizer dependency selections.
//! Adapter, ranking, documentation, and test changes are outside this boundary.

use std::sync::OnceLock;

use crate::config::INDEX_CONTENT_VERSION;

const MANIFEST_VERSION: u32 = 1;
const LOCKFILE: &str = include_str!("../Cargo.lock");

struct DerivationComponent {
    name: &'static str,
    version: u32,
    sources: &'static [(&'static str, &'static str)],
}

const COMPONENTS: &[DerivationComponent] = &[
    DerivationComponent {
        name: "syntax",
        version: 1,
        sources: &[
            ("src/parser/mod.rs", include_str!("parser/mod.rs")),
            ("src/parser/api.rs", include_str!("parser/api.rs")),
            (
                "src/parser/hierarchy.rs",
                include_str!("parser/hierarchy.rs"),
            ),
            ("src/parser/imports.rs", include_str!("parser/imports.rs")),
            ("src/parser/queries.rs", include_str!("parser/queries.rs")),
            (
                "src/parser/tree_sitter.rs",
                include_str!("parser/tree_sitter.rs"),
            ),
            ("src/parser/latex.rs", include_str!("parser/latex.rs")),
            ("src/parser/markdown.rs", include_str!("parser/markdown.rs")),
            (
                "src/parser/languages/csharp.rs",
                include_str!("parser/languages/csharp.rs"),
            ),
            (
                "src/parser/languages/css.rs",
                include_str!("parser/languages/css.rs"),
            ),
            (
                "src/parser/languages/html.rs",
                include_str!("parser/languages/html.rs"),
            ),
            (
                "src/parser/languages/javascript.rs",
                include_str!("parser/languages/javascript.rs"),
            ),
        ],
    },
    DerivationComponent {
        name: "text_and_tokens",
        version: 1,
        sources: &[
            ("src/text.rs", include_str!("text.rs")),
            ("src/tokens.rs", include_str!("tokens.rs")),
            ("src/indexer/prepare.rs", include_str!("indexer/prepare.rs")),
        ],
    },
    DerivationComponent {
        name: "imports_and_projections",
        version: 1,
        sources: &[
            (
                "src/indexer/import_resolution.rs",
                include_str!("indexer/import_resolution.rs"),
            ),
            ("src/repository/path.rs", include_str!("repository/path.rs")),
            (
                "src/storage/projections.rs",
                include_str!("storage/projections.rs"),
            ),
        ],
    },
];

const DEPENDENCY_OWNERS: &[&str] = &[
    "blake3",
    "pulldown-cmark",
    "regex",
    "regex-automata",
    "regex-syntax",
    "tiktoken-rs",
    "tree-sitter",
    "tree-sitter-c",
    "tree-sitter-c-sharp",
    "tree-sitter-cpp",
    "tree-sitter-css",
    "tree-sitter-go",
    "tree-sitter-html",
    "tree-sitter-java",
    "tree-sitter-javascript",
    "tree-sitter-language",
    "tree-sitter-php",
    "tree-sitter-python",
    "tree-sitter-ruby",
    "tree-sitter-rust",
    "tree-sitter-typescript",
];

/// Exact BLAKE3 identity of code and dependencies that can change persisted rows.
pub(crate) fn index_derivation_fingerprint() -> &'static str {
    static FINGERPRINT: OnceLock<String> = OnceLock::new();
    FINGERPRINT.get_or_init(compute_fingerprint)
}

fn compute_fingerprint() -> String {
    compute_fingerprint_with_source(None)
}

fn compute_fingerprint_with_source(source_override: Option<(&str, &str)>) -> String {
    let mut hasher = blake3::Hasher::new();
    hash_field(
        &mut hasher,
        "manifest_version",
        &MANIFEST_VERSION.to_le_bytes(),
    );
    hash_field(
        &mut hasher,
        "index_content_version",
        &INDEX_CONTENT_VERSION.to_le_bytes(),
    );
    for component in COMPONENTS {
        hash_field(&mut hasher, "component", component.name.as_bytes());
        hash_field(
            &mut hasher,
            "component_version",
            &component.version.to_le_bytes(),
        );
        for (path, source) in component.sources {
            hash_field(&mut hasher, "source_path", path.as_bytes());
            let source = source_override
                .filter(|(override_path, _)| override_path == path)
                .map_or(*source, |(_, source)| source);
            hash_field(&mut hasher, "source", source.as_bytes());
        }
    }
    for dependency in DEPENDENCY_OWNERS {
        hash_field(&mut hasher, "dependency", dependency.as_bytes());
        match locked_package(dependency) {
            Some(package) => hash_field(&mut hasher, "locked_package", package.as_bytes()),
            None => hash_field(&mut hasher, "missing_locked_package", dependency.as_bytes()),
        }
    }
    hasher.finalize().to_hex().to_string()
}

fn locked_package(name: &str) -> Option<&'static str> {
    LOCKFILE.split("[[package]]").skip(1).find(|package| {
        package
            .lines()
            .find_map(|line| line.trim().strip_prefix("name = \"")?.strip_suffix('"'))
            == Some(name)
    })
}

fn hash_field(hasher: &mut blake3::Hasher, name: &str, value: &[u8]) {
    hasher.update(&(name.len() as u64).to_le_bytes());
    hasher.update(name.as_bytes());
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_derivation_dependency_is_locked() {
        for dependency in DEPENDENCY_OWNERS {
            assert!(
                locked_package(dependency).is_some(),
                "missing derivation dependency {dependency}"
            );
        }
    }

    #[test]
    fn derivation_fingerprint_is_stable_and_full_width() {
        let fingerprint = index_derivation_fingerprint();
        assert_eq!(fingerprint.len(), 64);
        assert!(fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(fingerprint, compute_fingerprint());
    }

    #[test]
    fn same_version_derivation_source_changes_cannot_reuse_the_fingerprint() {
        let current = include_str!("parser/hierarchy.rs");
        let previous_semantics_fixture =
            format!("{current}\n// previous enclosing-owner semantics");

        assert_ne!(
            compute_fingerprint(),
            compute_fingerprint_with_source(Some((
                "src/parser/hierarchy.rs",
                &previous_semantics_fixture,
            )))
        );
    }
}
