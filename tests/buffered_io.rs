//! End-to-end smoke tests for `BufferedStdIo` in the interpreter path.
//!
//! The `cases_pipeline` suite exercises `put_byte`/`get_byte` on outputs up to
//! ~1.5 KiB; these tests cover the two regimes it misses: a single program
//! that emits more than the 4 KiB flush threshold, and a `,`-heavy program
//! that hits stdin EOF. Both scenarios are tested through the binary in
//! interpret mode so the real `BufferedStdIo` adaptor is in the loop.

use assert_cmd::cargo::CommandCargoExt;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

mod common;

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

/// Prints 'A' exactly 5000 times — more than one buffer page (4096) so the
/// mid-program auto-flush path runs at least once, plus the final
/// program-end flush via `RuntimeIo::flush`.
#[test]
fn interp_buffered_stdout_crosses_4kib_boundary() {
    let temp = TempDirGuard::new("amazingbf-buffered-io-out");
    let bf_path = temp.path.join("big.bf");

    // Cell 1 = 'A' (65), then 5000 `.`s on that cell.
    let mut src = String::from("++++++++[>++++++++<-]>+");
    src.extend(std::iter::repeat_n('.', 5000));
    fs::write(&bf_path, src).unwrap();

    let mut cmd = Command::cargo_bin("AmazingBF").unwrap();
    cmd.arg(&bf_path).arg("-q");
    let output = cmd.output().expect("spawn AmazingBF");

    assert!(
        output.status.success(),
        "interpreter failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout.len(), 5000);
    assert!(
        output.stdout.iter().all(|&b| b == b'A'),
        "stdout contained non-'A' byte"
    );
}

/// A `,` past EOF must yield 255 (convention shared with `StdIo` / the native
/// backend). Sends a single-byte stdin, then reads two bytes and echoes them;
/// the second read must round-trip as 255.
#[test]
fn interp_buffered_stdin_returns_255_on_eof() {
    let temp = TempDirGuard::new("amazingbf-buffered-io-eof");
    let bf_path = temp.path.join("eof.bf");
    let in_path = temp.path.join("eof.in");

    // Read into cell 0, print it; read again (should be 255) into cell 1,
    // print it. Net stdout = [first_byte, 255].
    fs::write(&bf_path, ",.>,.").unwrap();
    fs::write(&in_path, b"Q").unwrap();

    let mut cmd = Command::cargo_bin("AmazingBF").unwrap();
    cmd.arg(&bf_path).arg("-q");
    let output = common::run_with_optional_input(cmd, &in_path);

    assert!(
        output.status.success(),
        "interpreter failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, vec![b'Q', 255]);
}
