//! End-to-end coverage for interpreter hotspot diagnostics.

use assert_cmd::cargo::CommandCargoExt;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

struct TempDirGuard {
    path: PathBuf,
}

impl TempDirGuard {
    fn new(prefix: &str) -> Self {
        let unique = format!(
            "{}-{}-{}",
            prefix,
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let path = std::env::temp_dir().join(unique);
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn write_hotspot_case() -> (TempDirGuard, PathBuf, PathBuf) {
    let temp = TempDirGuard::new("amazingbf-interp-hotspots");
    let bf_path = temp.path.join("hot.bf");
    let input_path = temp.path.join("hot.in");
    fs::write(&bf_path, ",[.-]").unwrap();
    fs::write(&input_path, [3]).unwrap();
    (temp, bf_path, input_path)
}

#[test]
fn interp_debug_prints_tape_and_hotspot_report() {
    let (_temp, bf_path, input_path) = write_hotspot_case();

    let mut cmd = Command::cargo_bin("AmazingBF").unwrap();
    cmd.arg(&bf_path)
        .arg("-q")
        .arg("-O0")
        .arg("--interp-debug")
        .stdin(std::fs::File::open(input_path).unwrap());
    let output = cmd.output().expect("spawn AmazingBF");

    assert!(
        output.status.success(),
        "interpreter failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, vec![3, 2, 1]);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("[interp-debug] tape initial_cells="));
    assert!(stderr.contains("[interp-debug] hotspots total_opcode_dispatches="));
    assert!(stderr.contains("[interp-debug] top_opcodes"));
    assert!(stderr.contains("opcode=Add") || stderr.contains("opcode=MoveAdd"));
    assert!(stderr.contains("[interp-debug] top_loops"));
    assert!(stderr.contains("loop_start_pc="));
    assert!(stderr.contains("trips=2"));
}

#[test]
fn quiet_interpret_without_debug_does_not_print_hotspots() {
    let (_temp, bf_path, input_path) = write_hotspot_case();

    let mut cmd = Command::cargo_bin("AmazingBF").unwrap();
    cmd.arg(&bf_path)
        .arg("-q")
        .stdin(std::fs::File::open(input_path).unwrap());
    let output = cmd.output().expect("spawn AmazingBF");

    assert!(
        output.status.success(),
        "interpreter failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, vec![3, 2, 1]);
    assert!(
        output.stderr.is_empty(),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
