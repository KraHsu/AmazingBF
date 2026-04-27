//! End-to-end tests for JIT mode (`-m jit`).
//!
//! Each test compiles a `.bf` program via the JIT pipeline and compares
//! stdout against the expected `.out` file, mirroring `cases_pipeline.rs`.

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

fn run_jit_case(case: u32) {
    let root = project_root();
    let bf = root.join(format!("tests/cases/{case}.bf"));
    let expected = std::fs::read(root.join(format!("tests/cases/{case}.out")))
        .unwrap_or_else(|_| panic!("missing tests/cases/{case}.out"));
    let input = std::fs::read(root.join(format!("tests/cases/{case}.in"))).unwrap_or_default();

    let output = Command::new(amazingbf_bin())
        .args([bf.to_str().unwrap(), "-m", "jit", "-q"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if !input.is_empty()
                && let Some(ref mut stdin) = child.stdin
            {
                let _ = stdin.write_all(&input);
            }
            drop(child.stdin.take());
            child.wait_with_output()
        })
        .unwrap_or_else(|e| panic!("failed to run AmazingBF for case {case}: {e}"));

    assert!(
        output.status.success(),
        "case {case}: JIT exited with {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout,
        expected,
        "case {case}: JIT stdout mismatch\ngot:      {:?}\nexpected: {:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&expected)
    );
}

fn run_jit_opt_levels(case: u32) {
    let root = project_root();
    let bf = root.join(format!("tests/cases/{case}.bf"));
    let expected = std::fs::read(root.join(format!("tests/cases/{case}.out")))
        .unwrap_or_else(|_| panic!("missing tests/cases/{case}.out"));
    let input = std::fs::read(root.join(format!("tests/cases/{case}.in"))).unwrap_or_default();

    for level in ["0", "1", "2", "3"] {
        let output = Command::new(amazingbf_bin())
            .args([
                bf.to_str().unwrap(),
                "-m",
                "jit",
                "-q",
                &format!("-O{level}"),
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                if !input.is_empty()
                    && let Some(ref mut stdin) = child.stdin
                {
                    let _ = stdin.write_all(&input);
                }
                drop(child.stdin.take());
                child.wait_with_output()
            })
            .unwrap_or_else(|e| panic!("case {case} -O{level}: {e}"));

        assert!(
            output.status.success(),
            "case {case} -O{level}: exit {:?}\nstderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            output.stdout, expected,
            "case {case} -O{level}: stdout mismatch"
        );
    }
}

#[test]
fn jit_case_1() {
    run_jit_case(1);
}

#[test]
fn jit_case_2() {
    run_jit_case(2);
}

#[test]
fn jit_case_3() {
    run_jit_case(3);
}

#[test]
fn jit_case_4() {
    run_jit_case(4);
}

#[test]
fn jit_case_5() {
    run_jit_case(5);
}

#[test]
fn jit_all_opt_levels_case_1() {
    run_jit_opt_levels(1);
}
