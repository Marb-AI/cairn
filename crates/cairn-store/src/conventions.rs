//! Naming conventions per ecosystem.
//!
//! These belong in the declarative rule packs of architecture 1.1 layer B, and will
//! move there once that engine exists. They live here, in one place and clearly
//! labelled, so that "the core knows no language" stays checkable: this is the only
//! file in cairn-store that names a language.
//!
//! Note the difference from generated-code detection (7.3), which deliberately does
//! *not* trust filename patterns. Here patterns are the right signal, because test
//! runners themselves discover tests by exactly these conventions - pytest collects
//! `test_*.py`, `go test` compiles `*_test.go`. The convention is the contract.

/// Is this file a test, by the convention its own test runner uses?
///
/// Every list here is the rule pack's. It used to be two: the pack had one set of
/// suffixes and this function had another, and they drifted exactly as the directory
/// list below them had drifted before - the pack learned `.spec.tsx` and this did not,
/// so a React component whose only caller was `Thing.spec.tsx` counted as production
/// code and looked alive. One definition, in `rules/default.yaml`, where a repository
/// can also override it.
pub fn is_test_path(path: &str) -> bool {
    let rules = crate::rules::Rules::default();
    is_test_path_with(path, &rules.tests)
}

/// The same question against a specific pack, which is what a store with its own
/// `rules.yaml` holds.
pub fn is_test_path_with(path: &str, tests: &crate::rules::TestRules) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    // `go test` compiles `*_test.go`; pytest collects `test_*.py`; jest and vitest take
    // `*.test.*` and `*.spec.*`. The convention is the contract, which is why patterns
    // are the right signal here and the wrong one for generated code (7.3).
    if tests
        .file_suffixes
        .iter()
        .any(|s| file.ends_with(s.as_str()))
    {
        return true;
    }
    if tests
        .file_prefixes
        .iter()
        .any(|p| file.starts_with(p.as_str()))
    {
        // A prefix alone is not enough: `test_helper.go` is Go source, and only pytest
        // collects by prefix. Kept to the languages whose runners do.
        if file.ends_with(".py") || file.ends_with(".ts") || file.ends_with(".tsx") {
            return true;
        }
    }
    // Directory conventions, checked last: a helper inside a tests/ tree is still
    // test-only code even when its own name says nothing.
    tests
        .path_contains
        .iter()
        .any(|d| path.contains(d.as_str()))
        || tests
            .path_prefixes
            .iter()
            .any(|p| path.starts_with(p.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_runner_conventions() {
        assert!(is_test_path("srcgo/domains/x/handler_test.go"));
        assert!(is_test_path("srcpy/domains/x/test_order.py"));
        assert!(is_test_path("srcpy/domains/x/order_test.py"));
        assert!(is_test_path("web/src/x.test.ts"));
        assert!(is_test_path(
            "srcpy/domains/orders/grpc/tests/handlers/conftest.py"
        ));
    }

    /// The suffixes that were in the pack and not in this file. A React component's
    /// tests are `Thing.spec.tsx`, and every one of them counted as production code.
    #[test]
    fn recognises_the_suffixes_the_second_copy_had_missed() {
        for path in [
            "frontend/apps/auth/src/Thing.spec.tsx",
            "frontend/apps/auth/src/Thing.test.tsx",
            "frontend/apps/auth/src/thing.spec.jsx",
            "frontend/apps/auth/src/thing.test.jsx",
        ] {
            assert!(is_test_path(path), "{path} is a test by its runner's rule");
        }
    }

    #[test]
    fn leaves_ordinary_code_alone() {
        assert!(!is_test_path("srcgo/domains/x/handler.go"));
        assert!(!is_test_path("srcpy/domains/x/order.py"));
        // A word that merely contains "test" is not a convention.
        assert!(!is_test_path("srcpy/domains/x/latest.py"));
        assert!(!is_test_path("srcpy/domains/x/protest.py"));
        // A prefix without its language: only pytest collects by prefix, and
        // `test_helper.go` is ordinary Go source.
        assert!(!is_test_path("srcgo/domains/x/test_helper.go"));
    }
}

#[cfg(test)]
mod pack_backed {
    use super::*;

    #[test]
    fn a_directory_convention_added_to_the_pack_takes_effect_here() {
        // The two lists were separate and drifted: the pack learned `/__tests__/` and this
        // function did not, so a stub beside a spec file counted as production code. What
        // is asserted is not the contents of the list but that this reads it.
        for d in &crate::rules::Rules::default().tests.path_contains {
            let path = format!("apps/web/components{d}helper.ts");
            assert!(
                is_test_path(&path),
                "the pack names {d} as a test directory and {path} was not treated as one"
            );
        }
        assert!(!is_test_path("apps/web/components/helper.ts"));
    }
}
