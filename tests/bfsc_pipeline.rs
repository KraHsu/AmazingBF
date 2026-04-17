//! End-to-end tests for the BFS (Brainf Script) compiler.
//!
//! For each `tests/utils/*.bfs` file with a corresponding `.in` and `.out`,
//! this test: compiles via `bfsc`, runs the resulting BF through `AmazingBF`,
//! and compares stdout to the expected `.out`.

use assert_cmd::cargo::CommandCargoExt;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

mod common;

const UTILS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/cases_bfs");

fn bfs_cases() -> Vec<PathBuf> {
    let mut paths: Vec<_> = fs::read_dir(UTILS_DIR)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("bfs"))
        .collect();
    paths.sort();
    paths
}

#[test]
fn bfsc_cases_produce_correct_output() {
    let bfs_files = bfs_cases();
    assert!(!bfs_files.is_empty(), "no .bfs files found in {UTILS_DIR}");

    let mut failures = Vec::new();

    for bfs_file in &bfs_files {
        let stem = bfs_file.file_stem().unwrap().to_string_lossy().into_owned();
        let in_file = bfs_file.with_extension("in");
        let out_file = bfs_file.with_extension("out");

        if !out_file.is_file() {
            continue; // no expected output, skip
        }

        // Step 1: compile .bfs → BF text
        let mut compile_cmd = Command::cargo_bin("bfsc").unwrap();
        compile_cmd.arg(bfs_file);
        let compile_out = compile_cmd.output().unwrap();

        if !compile_out.status.success() {
            failures.push(format!(
                "{stem}: bfsc compile error\nstderr:\n{}",
                String::from_utf8_lossy(&compile_out.stderr)
            ));
            continue;
        }

        let bf_text = compile_out.stdout;

        // Step 2: write BF to a temp file
        let tmp_bf = std::env::temp_dir().join(format!("bfsc-test-{stem}.bf"));
        fs::write(&tmp_bf, &bf_text).expect("write temp .bf");

        // Step 3: run AmazingBF with the .in file as stdin
        let mut run_cmd = Command::cargo_bin("AmazingBF").unwrap();
        run_cmd.arg(&tmp_bf).arg("-q");

        let run_output = common::run_with_optional_input(run_cmd, &in_file);
        let _ = fs::remove_file(&tmp_bf);

        let expected = common::read_fixture_bytes(&out_file);

        if !run_output.status.success() {
            failures.push(format!(
                "{stem}: AmazingBF runtime error\nstderr:\n{}",
                String::from_utf8_lossy(&run_output.stderr)
            ));
            continue;
        }

        if run_output.stdout != expected {
            failures.push(format!(
                "{stem}: output mismatch\nexpected: {:?}\nactual:   {:?}",
                expected, run_output.stdout
            ));
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
