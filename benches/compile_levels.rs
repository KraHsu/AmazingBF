//! Compile + run timing bench over `tests/cases/*.bf`:
//! each case × `-O0..3` × [`TRIALS`] timed runs.
//!
//! Prints per-case compile/run mean/min/max and a grand-total table summing
//! per-case means for every opt level. On Unix, the first trial per cell
//! additionally captures max RSS via `/usr/bin/time -f "%e %M"`.
//!
//! Artifact correctness (ELF/PE layout, `.asm`/`.lst` contents, EOF stdout)
//! is checked by `tests/compile_artifacts.rs`; this bench does not re-validate
//! those properties — it only measures.
//!
//! Run: `cargo bench --bench compile_levels`.

#[path = "../tests/common.rs"]
mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
#[cfg(target_os = "windows")]
use std::thread;
#[cfg(target_os = "windows")]
use std::time::Duration;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const CASES_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/cases");
/// Repetitions per (case × `-O` level) for timing statistics.
const TRIALS: usize = 10;

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

/// GNU `time` (`%e` elapsed sec, `%M` max RSS KB). Returns `None` if missing or parse fails.
#[cfg(not(target_os = "windows"))]
fn gnu_time_elapsed_and_rss_kb(path: &Path) -> Option<(f64, u64)> {
    let s = fs::read_to_string(path).ok()?;
    let mut it = s.split_whitespace();
    let elapsed = it.next()?.parse().ok()?;
    let rss = it.next()?.parse().ok()?;
    Some((elapsed, rss))
}

fn mean_min_max(vals: &[f64]) -> (f64, f64, f64) {
    assert!(!vals.is_empty());
    let n = vals.len() as f64;
    let mean = vals.iter().sum::<f64>() / n;
    let min = vals.iter().copied().fold(f64::INFINITY, f64::min);
    let max = vals.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    (mean, min, max)
}

/// Run `AmazingBF compile` once (`-q` keeps huge-case logs off during the benchmark loop).
fn amazingbf_compile_output(
    amazingbf: &Path,
    bf_file: &Path,
    flag: &str,
    output_path: &Path,
) -> Output {
    Command::new(amazingbf)
        .arg("-q")
        .arg(bf_file)
        .arg("-m")
        .arg("compile")
        .arg("-O")
        .arg(flag)
        .arg("-o")
        .arg(output_path)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn AmazingBF: {e}"))
}

/// Large PE writes right after multi‑GB compiler work occasionally fail on Windows (AV indexing,
/// transient `ERROR_SHARING_VIOLATION`). One short retry keeps the bench from flaking.
fn amazingbf_compile_output_resilient(
    amazingbf: &Path,
    bf_file: &Path,
    flag: &str,
    output_path: &Path,
) -> Output {
    let out = amazingbf_compile_output(amazingbf, bf_file, flag, output_path);
    #[cfg(target_os = "windows")]
    if !out.status.success() {
        thread::sleep(Duration::from_millis(200));
        return amazingbf_compile_output(amazingbf, bf_file, flag, output_path);
    }
    out
}

fn assert_amazingbf_compile_ok(
    amazingbf: &Path,
    bf_file: &Path,
    flag: &str,
    output_path: &Path,
    case_name: &str,
) {
    let out = amazingbf_compile_output_resilient(amazingbf, bf_file, flag, output_path);
    assert!(
        out.status.success(),
        "compile failed case {case_name} -O{flag}\nexit code: {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[cfg(not(target_os = "windows"))]
fn run_executable_gnu_time_rss_kb(
    gnu_time: &Path,
    exe: &Path,
    input_path: &Path,
    time_out: &Path,
) -> Option<u64> {
    let mut cmd = Command::new(gnu_time);
    cmd.args(["-f", "%e %M", "-o"])
        .arg(time_out)
        .arg("--")
        .arg(exe)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let status = common::status_with_optional_input(cmd, input_path);
    if !status.success() {
        return None;
    }
    gnu_time_elapsed_and_rss_kb(time_out).map(|(_, rss)| rss)
}

#[cfg(not(target_os = "windows"))]
fn bench_elf() {
    let levels: &[(&str, &str)] = &[("0", "O0"), ("1", "O1"), ("2", "O2"), ("3", "O3")];
    let gnu_time = Path::new("/usr/bin/time");
    let have_gnu_time = gnu_time.is_file();

    let temp = TempDirGuard::new("amazingbf-compile-bench");
    let amazingbf = Path::new(env!("CARGO_BIN_EXE_AmazingBF"));

    let mut sum_compile_mean_by_level = [0.0f64; 4];
    let mut sum_run_mean_by_level = [0.0f64; 4];
    let case_count = case_paths().len();

    eprintln!();
    eprintln!(
        "compile_levels: {} cases × {} opt levels × {} trials (stdin from case N.in if present)",
        case_count,
        levels.len(),
        TRIALS
    );

    for bf_file in case_paths() {
        let name = bf_file.file_stem().unwrap().to_string_lossy().into_owned();
        let in_file = Path::new(CASES_DIR).join(format!("{name}.in"));

        eprintln!();
        eprintln!("--- case {name} ({}) ---", bf_file.display());

        let mut case_sum_compile_mean = 0.0f64;
        let mut case_sum_run_mean = 0.0f64;

        for (level_idx, (flag, label)) in levels.iter().enumerate() {
            let output_path = temp.path().join(format!("{name}_{}", flag));
            let time_compile = temp.path().join(format!("time_compile_{name}_{}", flag));
            let time_run = temp.path().join(format!("time_run_{name}_{}", flag));

            let mut compile_ms_samples = Vec::with_capacity(TRIALS);
            let mut run_ms_samples = Vec::with_capacity(TRIALS);
            let mut elf_len = 0usize;
            let mut asm_bytes = 0u64;
            let mut compile_rss_kb: Option<u64> = None;
            let mut run_rss_kb: Option<u64> = None;

            for trial in 0..TRIALS {
                let t_compile = Instant::now();
                let trial_compile_rss = if trial == 0 && have_gnu_time {
                    let status = Command::new(gnu_time)
                        .args(["-f", "%e %M", "-o"])
                        .arg(&time_compile)
                        .arg("--")
                        .arg(amazingbf)
                        .arg("-q")
                        .arg(&bf_file)
                        .arg("-m")
                        .arg("compile")
                        .arg("-O")
                        .arg(flag)
                        .arg("-o")
                        .arg(&output_path)
                        .stdin(Stdio::null())
                        .status()
                        .unwrap();
                    assert!(status.success(), "compile failed case {name} -O{flag}");
                    gnu_time_elapsed_and_rss_kb(&time_compile).map(|(_, r)| r)
                } else {
                    assert_amazingbf_compile_ok(amazingbf, &bf_file, flag, &output_path, &name);
                    None
                };
                compile_ms_samples.push(t_compile.elapsed().as_secs_f64() * 1000.0);
                if trial == 0 {
                    compile_rss_kb = trial_compile_rss;
                }

                if trial == 0 {
                    let elf_meta = fs::metadata(&output_path).unwrap();
                    let asm_path = output_path.with_extension("asm");
                    asm_bytes = fs::metadata(&asm_path).map(|m| m.len()).unwrap_or(0);
                    elf_len = elf_meta.len() as usize;
                }

                let t_run = Instant::now();
                let runtime_output =
                    common::run_with_optional_input(Command::new(&output_path), &in_file);
                run_ms_samples.push(t_run.elapsed().as_secs_f64() * 1000.0);

                if trial == 0 && have_gnu_time {
                    run_rss_kb =
                        run_executable_gnu_time_rss_kb(gnu_time, &output_path, &in_file, &time_run);
                }

                assert!(
                    runtime_output.status.success(),
                    "{label} {name} trial {trial} stderr:\n{}",
                    String::from_utf8_lossy(&runtime_output.stderr)
                );
            }

            let (c_mean, c_min, c_max) = mean_min_max(&compile_ms_samples);
            let (r_mean, r_min, r_max) = mean_min_max(&run_ms_samples);

            sum_compile_mean_by_level[level_idx] += c_mean;
            sum_run_mean_by_level[level_idx] += r_mean;
            case_sum_compile_mean += c_mean;
            case_sum_run_mean += r_mean;

            let hir_note = match *flag {
                "0" => "o0",
                "1" => "o1×1",
                "2" => "o2 fixpt",
                "3" => "o2 + O3 fold",
                _ => "",
            };

            eprintln!(
                "{:<5} {:>7} {:>7} | compile_ms mean/min/max: {:>6.3} / {:>6.3} / {:>6.3} | run_ms mean/min/max: {:>6.3} / {:>6.3} / {:>6.3} | rss_compile {:>8} rss_run {:>8} | {}",
                label,
                elf_len,
                asm_bytes,
                c_mean,
                c_min,
                c_max,
                r_mean,
                r_min,
                r_max,
                compile_rss_kb
                    .map(|k| format!("{}k", k))
                    .unwrap_or_else(|| "n/a".into()),
                run_rss_kb
                    .map(|k| format!("{}k", k))
                    .unwrap_or_else(|| "n/a".into()),
                hir_note,
            );
        }

        eprintln!(
            "    [case {name} Σ means over O0..O3] compile {case_sum_compile_mean:.3} ms | run {case_sum_run_mean:.3} ms"
        );

        // JIT mode: measures combined compile+execute in a single process.
        let jit_levels: &[(&str, &str)] = &[
            ("0", "JIT-O0"),
            ("1", "JIT-O1"),
            ("2", "JIT-O2"),
            ("3", "JIT-O3"),
        ];
        for (flag, label) in jit_levels {
            let mut jit_ms_samples = Vec::with_capacity(TRIALS);
            for _trial in 0..TRIALS {
                let t = Instant::now();
                let mut cmd = Command::new(amazingbf);
                cmd.arg("-q")
                    .arg(&bf_file)
                    .arg("-m")
                    .arg("jit")
                    .arg("-O")
                    .arg(flag);
                let output = common::run_with_optional_input(cmd, &in_file);
                jit_ms_samples.push(t.elapsed().as_secs_f64() * 1000.0);
                assert!(
                    output.status.success(),
                    "{label} {name} jit failed:\n{}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            let (j_mean, j_min, j_max) = mean_min_max(&jit_ms_samples);
            eprintln!(
                "{:<8} jit_ms mean/min/max: {:>6.3} / {:>6.3} / {:>6.3}",
                label, j_mean, j_min, j_max,
            );
        }

        // Tiered JIT mode (F1b-P2): interpreter with hot-loop dispatch.
        let tiered_levels: &[(&str, &str)] =
            &[("1", "TIER-O1"), ("2", "TIER-O2"), ("3", "TIER-O3")];
        for (flag, label) in tiered_levels {
            let mut t_ms_samples = Vec::with_capacity(TRIALS);
            for _trial in 0..TRIALS {
                let t = Instant::now();
                let mut cmd = Command::new(amazingbf);
                cmd.arg("-q")
                    .arg(&bf_file)
                    .arg("-m")
                    .arg("tiered")
                    .arg("-O")
                    .arg(flag);
                let output = common::run_with_optional_input(cmd, &in_file);
                t_ms_samples.push(t.elapsed().as_secs_f64() * 1000.0);
                assert!(
                    output.status.success(),
                    "{label} {name} tiered failed:\n{}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            let (mean, min, max) = mean_min_max(&t_ms_samples);
            eprintln!(
                "{:<8} tier_ms mean/min/max: {:>6.3} / {:>6.3} / {:>6.3}",
                label, mean, min, max,
            );
        }
    }

    eprintln!();
    eprintln!("=== TOTALS (sum of per-case mean times over {case_count} cases) ===");
    eprintln!(
        "{:<5} {:>22} {:>22}",
        "lvl", "sum_compile_mean_ms", "sum_run_mean_ms"
    );
    for (idx, (_, label)) in levels.iter().enumerate() {
        eprintln!(
            "{:<5} {:>22.3} {:>22.3}",
            label, sum_compile_mean_by_level[idx], sum_run_mean_by_level[idx]
        );
    }
    let grand_compile: f64 = sum_compile_mean_by_level.iter().sum();
    let grand_run: f64 = sum_run_mean_by_level.iter().sum();
    eprintln!("{:<5} {:>22.3} {:>22.3}", "ALL_O", grand_compile, grand_run);

    if !have_gnu_time {
        eprintln!(
            "(install GNU /usr/bin/time for rss_compile / rss_run; wall times still measured)"
        );
    }
}

#[cfg(target_os = "windows")]
fn bench_pe() {
    let levels: &[(&str, &str)] = &[("0", "O0"), ("1", "O1"), ("2", "O2"), ("3", "O3")];

    let temp = TempDirGuard::new("amazingbf-compile-bench");
    let amazingbf = Path::new(env!("CARGO_BIN_EXE_AmazingBF"));

    let mut sum_compile_mean_by_level = [0.0f64; 4];
    let mut sum_run_mean_by_level = [0.0f64; 4];
    let case_count = case_paths().len();

    eprintln!();
    eprintln!(
        "compile_levels(windows): {} cases × {} opt levels × {} trials (stdin from case N.in if present)",
        case_count,
        levels.len(),
        TRIALS
    );

    for bf_file in case_paths() {
        let name = bf_file.file_stem().unwrap().to_string_lossy().into_owned();
        let in_file = Path::new(CASES_DIR).join(format!("{name}.in"));

        eprintln!();
        eprintln!("--- case {name} ({}) ---", bf_file.display());

        let mut case_sum_compile_mean = 0.0f64;
        let mut case_sum_run_mean = 0.0f64;

        for (level_idx, (flag, label)) in levels.iter().enumerate() {
            let output_path = temp.path().join(format!("{name}_{}.exe", flag));

            let mut compile_ms_samples = Vec::with_capacity(TRIALS);
            let mut run_ms_samples = Vec::with_capacity(TRIALS);
            let mut pe_len = 0usize;
            let mut asm_bytes = 0u64;

            for trial in 0..TRIALS {
                let t_compile = Instant::now();
                assert_amazingbf_compile_ok(amazingbf, &bf_file, flag, &output_path, &name);
                compile_ms_samples.push(t_compile.elapsed().as_secs_f64() * 1000.0);

                if trial == 0 {
                    let exe_meta = fs::metadata(&output_path).unwrap();
                    let asm_path = output_path.with_extension("asm");
                    asm_bytes = fs::metadata(&asm_path).map(|m| m.len()).unwrap_or(0);
                    pe_len = exe_meta.len() as usize;
                }

                let t_run = Instant::now();
                let runtime_output =
                    common::run_with_optional_input(Command::new(&output_path), &in_file);
                run_ms_samples.push(t_run.elapsed().as_secs_f64() * 1000.0);

                assert!(
                    runtime_output.status.success(),
                    "{label} {name} trial {trial} stderr:\n{}",
                    String::from_utf8_lossy(&runtime_output.stderr)
                );
            }

            let (c_mean, c_min, c_max) = mean_min_max(&compile_ms_samples);
            let (r_mean, r_min, r_max) = mean_min_max(&run_ms_samples);

            sum_compile_mean_by_level[level_idx] += c_mean;
            sum_run_mean_by_level[level_idx] += r_mean;
            case_sum_compile_mean += c_mean;
            case_sum_run_mean += r_mean;

            let hir_note = match *flag {
                "0" => "o0",
                "1" => "o1×1",
                "2" => "o2 fixpt",
                "3" => "o2 + O3 fold",
                _ => "",
            };

            eprintln!(
                "{:<5} {:>7} {:>7} | compile_ms mean/min/max: {:>6.3} / {:>6.3} / {:>6.3} | run_ms mean/min/max: {:>6.3} / {:>6.3} / {:>6.3} | {}",
                label, pe_len, asm_bytes, c_mean, c_min, c_max, r_mean, r_min, r_max, hir_note,
            );
        }

        eprintln!(
            "    [case {name} Σ means over O0..O3] compile {case_sum_compile_mean:.3} ms | run {case_sum_run_mean:.3} ms"
        );
    }

    eprintln!();
    eprintln!("=== TOTALS (sum of per-case mean times over {case_count} cases) ===");
    eprintln!(
        "{:<5} {:>22} {:>22}",
        "lvl", "sum_compile_mean_ms", "sum_run_mean_ms"
    );
    for (idx, (_, label)) in levels.iter().enumerate() {
        eprintln!(
            "{:<5} {:>22.3} {:>22.3}",
            label, sum_compile_mean_by_level[idx], sum_run_mean_by_level[idx]
        );
    }
    let grand_compile: f64 = sum_compile_mean_by_level.iter().sum();
    let grand_run: f64 = sum_run_mean_by_level.iter().sum();
    eprintln!("{:<5} {:>22.3} {:>22.3}", "ALL_O", grand_compile, grand_run);
}

fn main() {
    #[cfg(not(target_os = "windows"))]
    bench_elf();
    #[cfg(target_os = "windows")]
    bench_pe();
}
