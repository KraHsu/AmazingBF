use assert_cmd::cargo::CommandCargoExt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const CASES_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/cases");

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

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn case_paths() -> Vec<PathBuf> {
    let mut paths: Vec<_> = fs::read_dir(CASES_DIR)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("bf"))
        .collect();
    paths.sort();
    assert!(
        !paths.is_empty(),
        "no .bf files found in {}",
        Path::new(CASES_DIR).display()
    );
    paths
}

fn run_with_optional_input(mut cmd: Command, input_path: &Path) -> Output {
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().unwrap();
    if input_path.is_file() {
        let input = fs::read(input_path).unwrap();
        child.stdin.take().unwrap().write_all(&input).unwrap();
    } else {
        drop(child.stdin.take());
    }

    child.wait_with_output().unwrap()
}

#[test]
fn interp_cases_match_expected_output() {
    let mut failures = Vec::new();

    for bf_file in case_paths() {
        let name = bf_file.file_stem().unwrap().to_string_lossy().into_owned();
        let in_file = Path::new(CASES_DIR).join(format!("{name}.in"));
        let out_file = Path::new(CASES_DIR).join(format!("{name}.out"));
        assert!(out_file.is_file(), "[interp] {name}: missing .out");

        let mut cmd = Command::cargo_bin("AmazingBF").unwrap();
        cmd.arg(&bf_file).arg("-q");

        let output = run_with_optional_input(cmd, &in_file);
        let expected = fs::read(&out_file).unwrap();

        if !output.status.success() {
            failures.push(format!(
                "[interp] {name}: runtime error\nstderr:\n{}",
                String::from_utf8_lossy(&output.stderr)
            ));
            continue;
        }

        if output.stdout != expected {
            failures.push(format!(
                "[interp] {name}: output mismatch\nexpected: {:?}\nactual:   {:?}",
                expected, output.stdout
            ));
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn compile_cases_match_expected_output_and_emit_artifacts() {
    let temp = TempDirGuard::new("amazingbf-cases");
    let bin_tmp_dir = temp.path().join("bin");
    fs::create_dir_all(&bin_tmp_dir).unwrap();

    let mut failures = Vec::new();

    for bf_file in case_paths() {
        let name = bf_file.file_stem().unwrap().to_string_lossy().into_owned();
        let in_file = Path::new(CASES_DIR).join(format!("{name}.in"));
        let out_file = Path::new(CASES_DIR).join(format!("{name}.out"));
        let exe_file = bin_tmp_dir.join(&name);
        let asm_file = exe_file.with_extension("asm");
        let lst_file = exe_file.with_extension("lst");
        assert!(out_file.is_file(), "[compile] {name}: missing .out");

        let mut compile_cmd = Command::cargo_bin("AmazingBF").unwrap();
        compile_cmd
            .arg(&bf_file)
            .arg("-q")
            .arg("-m")
            .arg("compile")
            .arg("-o")
            .arg(&exe_file);

        let compile_output = compile_cmd.output().unwrap();
        if !compile_output.status.success() {
            failures.push(format!(
                "[compile] {name}: compile error\nstderr:\n{}",
                String::from_utf8_lossy(&compile_output.stderr)
            ));
            continue;
        }

        let artifacts_ok = [(&exe_file, "executable"), (&asm_file, "asm"), (&lst_file, "lst")]
            .into_iter()
            .all(|(path, _)| fs::metadata(path).map(|meta| meta.len() > 0).unwrap_or(false));
        if !artifacts_ok {
            failures.push(format!("[compile] {name}: missing compile artifacts"));
            continue;
        }

        let runtime_output = run_with_optional_input(Command::new(&exe_file), &in_file);
        let expected = fs::read(&out_file).unwrap();

        if !runtime_output.status.success() {
            failures.push(format!(
                "[compile] {name}: compiled program runtime error\nstderr:\n{}",
                String::from_utf8_lossy(&runtime_output.stderr)
            ));
            continue;
        }

        if runtime_output.stdout != expected {
            failures.push(format!(
                "[compile] {name}: output mismatch\nexpected: {:?}\nactual:   {:?}",
                expected, runtime_output.stdout
            ));
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
