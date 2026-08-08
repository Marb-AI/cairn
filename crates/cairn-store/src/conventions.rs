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
pub fn is_test_path(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    // Go: the toolchain only compiles `*_test.go` into the test binary.
    if file.ends_with("_test.go") {
        return true;
    }
    // Python: pytest's default collection patterns.
    if file.starts_with("test_") && file.ends_with(".py") {
        return true;
    }
    if file.ends_with("_test.py") {
        return true;
    }
    // JS/TS: jest and vitest defaults.
    for suffix in [".test.ts", ".test.tsx", ".test.js", ".spec.ts", ".spec.js"] {
        if file.ends_with(suffix) {
            return true;
        }
    }
    // Directory conventions, checked last: a helper inside a tests/ tree is still
    // test-only code even when its own name says nothing.
    //
    // The list is the rule pack's, not a second copy of it. It was a second copy, and the
    // two drifted the moment a JavaScript repository arrived: the pack could be taught
    // `/__tests__/` and this could not, so stubs and fixtures under it stayed production
    // code and kept 55 symbols looking alive whose only caller was a test.
    let dirs = &crate::rules::Rules::default().tests.path_contains;
    dirs.iter().any(|d| path.contains(d.as_str())) || path.starts_with("tests/")
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

    #[test]
    fn leaves_ordinary_code_alone() {
        assert!(!is_test_path("srcgo/domains/x/handler.go"));
        assert!(!is_test_path("srcpy/domains/x/order.py"));
        // A word that merely contains "test" is not a convention.
        assert!(!is_test_path("srcpy/domains/x/latest.py"));
        assert!(!is_test_path("srcpy/domains/x/protest.py"));
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
