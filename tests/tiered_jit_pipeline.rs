//! End-to-end tests for the F1b tiered JIT (`-m tiered`).
//!
//! Each test runs a `.bf` program through the interpreter with `-m tiered`
//! at a deliberately low `--jit-threshold` (so even modestly hot loops
//! cross the threshold and exercise the JIT dispatch path), then compares
//! stdout against the expected `.out` and against the pure-interpreter
//! output. The expected-file check guards against output regressions; the
//! cross-mode equality check guards against silent JIT mistranslations.

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::process::Command;

fn project_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn amazingbf_bin() -> PathBuf {
    let mut p = project_root();
    p.push("target");
    p.push(if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    });
    p.push("AmazingBF");
    p
}

fn run_with_args(args: &[&str], input: &[u8]) -> (Vec<u8>, Vec<u8>, Option<i32>) {
    let output = Command::new(amazingbf_bin())
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if !input.is_empty()
                && let Some(ref mut stdin) = child.stdin
            {
                let _ = stdin.write_all(input);
            }
            drop(child.stdin.take());
            child.wait_with_output()
        })
        .unwrap_or_else(|e| panic!("failed to run AmazingBF: {e}"));
    (output.stdout, output.stderr, output.status.code())
}

fn case_paths(case: u32) -> (PathBuf, Vec<u8>, Vec<u8>) {
    let root = project_root();
    let bf = root.join(format!("tests/cases/{case}.bf"));
    let expected = std::fs::read(root.join(format!("tests/cases/{case}.out")))
        .unwrap_or_else(|_| panic!("missing tests/cases/{case}.out"));
    let input = std::fs::read(root.join(format!("tests/cases/{case}.in"))).unwrap_or_default();
    (bf, expected, input)
}

fn run_tiered_case(case: u32, threshold: u64) {
    let (bf, expected, input) = case_paths(case);
    let bf_str = bf.to_str().unwrap();
    let threshold_str = format!("--jit-threshold={threshold}");

    let (tiered_stdout, tiered_stderr, tiered_code) =
        run_with_args(&[bf_str, "-m", "tiered", "-q", &threshold_str], &input);
    assert_eq!(
        tiered_code,
        Some(0),
        "case {case}: tiered exited non-zero ({tiered_code:?})\nstderr: {}",
        String::from_utf8_lossy(&tiered_stderr)
    );
    assert_eq!(
        tiered_stdout, expected,
        "case {case}: tiered stdout differs from expected"
    );

    // Cross-check against pure interpreter output: any silent JIT mistranslation
    // would slip past the expected-file check if both modes happened to break
    // the same way (e.g. a shared bug in HIR optimization), so compare directly.
    let (interp_stdout, _, interp_code) = run_with_args(&[bf_str, "-q"], &input);
    assert_eq!(
        interp_code,
        Some(0),
        "case {case}: interpret exited non-zero ({interp_code:?})"
    );
    assert_eq!(
        tiered_stdout, interp_stdout,
        "case {case}: tiered output differs from interpret output"
    );
}

#[test]
fn tiered_case_1() {
    run_tiered_case(1, 1);
}

#[test]
fn tiered_case_2() {
    run_tiered_case(2, 1);
}

#[test]
fn tiered_case_3() {
    run_tiered_case(3, 1);
}

#[test]
fn tiered_case_4() {
    run_tiered_case(4, 1);
}

#[test]
fn tiered_case_5() {
    run_tiered_case(5, 1);
}

#[test]
fn tiered_case_6() {
    run_tiered_case(6, 1);
}

#[test]
fn tiered_case_7() {
    run_tiered_case(7, 1);
}

#[test]
fn tiered_case_8() {
    run_tiered_case(8, 1);
}

/// With a high threshold no loop is ever marked hot, so every byte must come
/// from the interpreter — verifying the gating logic doesn't perturb the
/// non-JIT path.
#[test]
fn tiered_high_threshold_matches_interpret_for_all_cases() {
    for case in 1..=8 {
        run_tiered_case(case, u64::MAX);
    }
}
