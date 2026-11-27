//! Calls that must not compile: a modal without its dismiss handler. Each case and the error it
//! expects is under `tests/compile-fail/`; `TRYBUILD=overwrite` rewrites the expected errors.

#[test]
fn fr_l32_fr_l26_calls_that_must_not_compile() {
    trybuild::TestCases::new().compile_fail("tests/compile-fail/*.rs");
}
