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
    path.contains("/tests/") || path.starts_with("tests/") || path.contains("/testdata/")
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
