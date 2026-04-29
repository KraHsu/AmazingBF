//! Self-hosting BF interpreter end-to-end test.
//!
//! Compiles `examples/bf_self_host.bfs` to BF, then for each small fixture
//! BF program in `tests/cases/`, feeds `<program>!<input>` to the compiled
//! interpreter (running under `AmazingBF`) and checks the output against
//! the matching `.out`.
//!
//! Cases are gated on program length ≤ 255 — the interpreter's encoded
//! program buffer is sized for `bfsc`'s u8-indexed arrays. Anything bigger
//! lives outside the supported envelope and is skipped here, not failed.

use assert_cmd::cargo::CommandCargoExt;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[allow(dead_code)]
mod common;

const CASES_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/cases");
const BFS_SRC: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/bf_self_host.bfs");

const PROG_BUDGET: usize = 255;

fn compile_self_host_to_bf() -> PathBuf {
    let mut compile_cmd = Command::cargo_bin("bfsc").unwrap();
    compile_cmd.arg(BFS_SRC);
    let out = compile_cmd.output().expect("spawn bfsc");
    assert!(
        out.status.success(),
        "bfsc failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let tmp = std::env::temp_dir().join("bf_self_host.bf");
    fs::write(&tmp, &out.stdout).expect("write tmp bf");
    tmp
}

fn filter_bf(prog: &[u8]) -> Vec<u8> {
    prog.iter()
        .copied()
        .filter(|b| matches!(b, b'+' | b'-' | b'<' | b'>' | b'.' | b',' | b'[' | b']'))
        .collect()
}

#[test]
fn self_host_runs_small_fixtures() {
    let bfi_path = compile_self_host_to_bf();

    let mut cases: Vec<PathBuf> = fs::read_dir(CASES_DIR)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("bf"))
        .collect();
    cases.sort();

    let mut ran = 0usize;
    let mut failures = Vec::new();

    for case in &cases {
        let stem = case.file_stem().unwrap().to_string_lossy().into_owned();
        let prog_raw = fs::read(case).expect("read .bf fixture");
        let prog = filter_bf(&prog_raw);
        if prog.len() > PROG_BUDGET {
            continue; // outside the self-host envelope
        }
        let in_path = case.with_extension("in");
        let out_path = case.with_extension("out");
        if !out_path.is_file() {
            continue;
        }
        let input = if in_path.is_file() {
            common::read_fixture_bytes(&in_path)
        } else {
            Vec::new()
        };

        // Build stdin: <program>!<input>
        let mut composed = prog.clone();
        composed.push(b'!');
        composed.extend_from_slice(&input);

        let mut run_cmd = Command::cargo_bin("AmazingBF").unwrap();
        run_cmd
            .arg(&bfi_path)
            .arg("-q")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = run_cmd.spawn().expect("spawn AmazingBF");
        {
            let mut stdin = child.stdin.take().expect("stdin pipe");
            stdin.write_all(&composed).expect("write stdin");
            stdin.flush().expect("flush stdin");
        }
        let output = child.wait_with_output().expect("wait AmazingBF");
        if !output.status.success() {
            failures.push(format!(
                "{stem}: AmazingBF runtime error\nstderr:\n{}",
                String::from_utf8_lossy(&output.stderr)
            ));
            continue;
        }
        let expected = common::read_fixture_bytes(&out_path);
        if output.stdout != expected {
            failures.push(format!(
                "{stem}: output mismatch\nexpected: {:?}\nactual:   {:?}",
                expected, output.stdout
            ));
            continue;
        }
        ran += 1;
    }

    let _ = fs::remove_file(&bfi_path);

    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    assert!(
        ran >= 3,
        "expected at least 3 cases under {PROG_BUDGET} chars, ran {ran}"
    );
}
