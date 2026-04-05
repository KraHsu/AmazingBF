//! Shared helpers for integration tests that run binaries against fixture input/output files.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::thread::{self, JoinHandle};

fn normalize_fixture_newlines(bytes: Vec<u8>) -> Vec<u8> {
    #[cfg(target_os = "windows")]
    {
        let mut normalized = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
                normalized.push(b'\n');
                i += 2;
            } else {
                normalized.push(bytes[i]);
                i += 1;
            }
        }
        normalized
    }

    #[cfg(not(target_os = "windows"))]
    {
        bytes
    }
}

pub fn read_fixture_bytes(path: &Path) -> Vec<u8> {
    normalize_fixture_newlines(fs::read(path).expect("read fixture file"))
}

fn spawn_with_optional_input(
    mut cmd: Command,
    input_path: &Path,
) -> (Child, Option<JoinHandle<()>>) {
    cmd.stdin(Stdio::piped());

    let mut child = cmd.spawn().expect("spawn child");
    let writer = if input_path.is_file() {
        let input = read_fixture_bytes(input_path);
        let mut stdin = child.stdin.take().expect("child stdin pipe");
        Some(thread::spawn(move || {
            stdin.write_all(&input).expect("write child stdin");
            stdin.flush().expect("flush child stdin");
            drop(stdin);
        }))
    } else {
        drop(child.stdin.take());
        None
    };

    (child, writer)
}

fn join_writer(writer: Option<JoinHandle<()>>) {
    if let Some(writer) = writer {
        writer.join().expect("stdin writer thread");
    }
}

/// Run `cmd` with optional stdin from `input_path` (if that path is a file).
///
/// Stdin is written and **closed on a dedicated thread** so the child always sees OS EOF
/// after the file bytes, even while the parent is blocked in `wait_with_output` draining
/// stdout/stderr. Some BF programs only exit once `read` returns 0 (EOF).
pub fn run_with_optional_input(mut cmd: Command, input_path: &Path) -> Output {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let (child, writer) = spawn_with_optional_input(cmd, input_path);
    let output = child.wait_with_output().expect("wait_with_output");
    join_writer(writer);
    output
}

/// Run `cmd` with optional stdin from `input_path` and wait only for exit status.
///
/// This is used by wrappers like `/usr/bin/time` that should inherit the same case input and
/// EOF behavior as the main execution path, without forcing stdout/stderr to be captured.
#[allow(dead_code)]
pub fn status_with_optional_input(cmd: Command, input_path: &Path) -> ExitStatus {
    let (mut child, writer) = spawn_with_optional_input(cmd, input_path);
    let status = child.wait().expect("wait child");
    join_writer(writer);
    status
}
