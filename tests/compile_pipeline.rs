//! Compile-mode benchmark over `tests/cases/*.bf`: each case × `-O0..3` × [`TRIALS`] timed runs.
//!
//! The main test is `#[ignore]` by default (can take many minutes). Run:
//! `cargo test --test compile_pipeline -- --ignored --nocapture`
//! To include it in every `cargo test`, delete the `#[ignore]` on that test.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;


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

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn extend_hex_bytes(out: &mut Vec<u8>, field: &str) {
    for byte in field.split_whitespace() {
        out.push(u8::from_str_radix(byte, 16).unwrap());
    }
}

fn is_hex_listing_char(byte: u8) -> bool {
    byte.is_ascii_hexdigit() || byte == b' '
}

fn parse_hex_listing_bytes(listing: &str) -> Vec<u8> {
    let mut bytes = Vec::new();

    for line in listing.lines() {
        if let Some(rest) = line.strip_prefix("0x") {
            let Some((_, after_colon)) = rest.split_once(':') else {
                continue;
            };
            let Some((hex_field, _)) = after_colon.trim_start().split_once("  ") else {
                continue;
            };
            extend_hex_bytes(&mut bytes, hex_field);
            continue;
        }

        if let Some(rest) = line.strip_prefix("         ") {
            let trimmed = rest.trim();
            if !trimmed.is_empty() && trimmed.bytes().all(is_hex_listing_char) {
                extend_hex_bytes(&mut bytes, trimmed);
            }
        }
    }

    bytes
}

/// GNU `time` (`%e` elapsed sec, `%M` max RSS KB). Returns `None` if missing or parse fails.
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
#[ignore = "slow: cargo test --test compile_pipeline -- --ignored --nocapture"]
#[test]
fn compile_mode_emits_rx_elf_artifacts_and_preserves_eof_semantics() {
    let levels: &[(&str, &str)] = &[("0", "O0"), ("1", "O1"), ("2", "O2"), ("3", "O3")];
    let gnu_time = Path::new("/usr/bin/time");
    let have_gnu_time = gnu_time.is_file();

    let temp = TempDirGuard::new("amazingbf-compile");
    let amazingbf = Path::new(env!("CARGO_BIN_EXE_AmazingBF"));

    let mut sum_compile_mean_by_level = [0.0f64; 4];
    let mut sum_run_mean_by_level = [0.0f64; 4];
    let case_count = case_paths().len();

    eprintln!();
    eprintln!(
        "compile_pipeline: {} cases × {} opt levels × {} trials (stdin from case N.in if present; stdout vs N.out)",
        case_count,
        levels.len(),
        TRIALS
    );

    for bf_file in case_paths() {
        let name = bf_file.file_stem().unwrap().to_string_lossy().into_owned();
        let in_file = Path::new(CASES_DIR).join(format!("{name}.in"));
        let out_file = Path::new(CASES_DIR).join(format!("{name}.out"));
        assert!(out_file.is_file(), "[compile_pipeline] {name}: missing .out");
        let expected = common::read_fixture_bytes(&out_file);

        eprintln!();
        eprintln!("--- case {name} ({}) ---", bf_file.display());

        let mut case_sum_compile_mean = 0.0f64;
        let mut case_sum_run_mean = 0.0f64;

        for (level_idx, (flag, label)) in levels.iter().enumerate() {
            let output_path = temp.path().join(format!("{name}_eof_{}", flag));
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
                    assert!(
                        Command::new(amazingbf)
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
                            .unwrap()
                            .success(),
                        "compile failed case {name} -O{flag}"
                    );
                    None
                };
                compile_ms_samples.push(t_compile.elapsed().as_secs_f64() * 1000.0);
                if trial == 0 {
                    compile_rss_kb = trial_compile_rss;
                }

                let asm_path = output_path.with_extension("asm");
                let lst_path = output_path.with_extension("lst");

                if trial == 0 {
                    let elf = fs::read(&output_path).unwrap();
                    let asm_listing = fs::read_to_string(&asm_path).unwrap();
                    let hex_listing = fs::read_to_string(&lst_path).unwrap();
                    let metadata = fs::metadata(&output_path).unwrap();
                    asm_bytes = fs::metadata(&asm_path).map(|m| m.len()).unwrap_or(0);
                    let entry = read_u64(&elf, 24);
                    let base_vaddr = read_u64(&elf, 80);
                    let text_offset = usize::try_from(entry - base_vaddr).unwrap();
                    let elf_text = &elf[text_offset..];

                    assert_eq!(&elf[0..4], b"\x7FELF", "{label} {name}");
                    assert_eq!(read_u32(&elf, 64), 1, "{label} {name}");
                    assert_eq!(read_u32(&elf, 68), 0x5, "{label} {name}");
                    assert!(
                        metadata.permissions().mode() & 0o111 != 0,
                        "{label} {name}"
                    );
                    assert!(asm_path.is_file(), "{label} {name}");
                    assert!(lst_path.is_file(), "{label} {name}");
                    assert!(!asm_listing.trim().is_empty(), "{label} {name}");
                    assert!(asm_listing.contains("; === Brainfuck x86_64 Assembly Listing ==="));
                    assert!(hex_listing.contains("; === Brainfuck x86_64 Hex Listing ==="));
                    assert_eq!(parse_hex_listing_bytes(&hex_listing), elf_text, "{label} {name}");
                    elf_len = elf.len();
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
                assert_eq!(
                    runtime_output.stdout, expected,
                    "{label} {name} trial {trial}"
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
    eprintln!(
        "{:<5} {:>22.3} {:>22.3}",
        "ALL_O", grand_compile, grand_run
    );

    if !have_gnu_time {
        eprintln!(
            "(install GNU /usr/bin/time for rss_compile / rss_run; wall times still measured)"
        );
    }
}

#[cfg(target_os = "windows")]
#[ignore = "slow: cargo test --test compile_pipeline -- --ignored --nocapture"]
#[test]
fn compile_mode_emits_rx_pe_artifacts_and_preserves_eof_semantics() {
    let levels: &[(&str, &str)] = &[("0", "O0"), ("1", "O1"), ("2", "O2"), ("3", "O3")];

    let temp = TempDirGuard::new("amazingbf-compile");
    let amazingbf = Path::new(env!("CARGO_BIN_EXE_AmazingBF"));

    let mut sum_compile_mean_by_level = [0.0f64; 4];
    let mut sum_run_mean_by_level = [0.0f64; 4];
    let case_count = case_paths().len();

    eprintln!();
    eprintln!(
        "compile_pipeline(windows): {} cases × {} opt levels × {} trials (stdin from case N.in if present; stdout vs N.out)",
        case_count,
        levels.len(),
        TRIALS
    );

    for bf_file in case_paths() {
        let name = bf_file.file_stem().unwrap().to_string_lossy().into_owned();
        let in_file = Path::new(CASES_DIR).join(format!("{name}.in"));
        let out_file = Path::new(CASES_DIR).join(format!("{name}.out"));
        assert!(out_file.is_file(), "[compile_pipeline] {name}: missing .out");
        let expected = common::read_fixture_bytes(&out_file);

        eprintln!();
        eprintln!("--- case {name} ({}) ---", bf_file.display());

        let mut case_sum_compile_mean = 0.0f64;
        let mut case_sum_run_mean = 0.0f64;

        for (level_idx, (flag, label)) in levels.iter().enumerate() {
            let output_path = temp.path().join(format!("{name}_eof_{}.exe", flag));

            let mut compile_ms_samples = Vec::with_capacity(TRIALS);
            let mut run_ms_samples = Vec::with_capacity(TRIALS);
            let mut pe_len = 0usize;
            let mut asm_bytes = 0u64;

            for trial in 0..TRIALS {
                let t_compile = Instant::now();
                assert!(
                    Command::new(amazingbf)
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
                        .unwrap()
                        .success(),
                    "compile failed case {name} -O{flag}"
                );
                compile_ms_samples.push(t_compile.elapsed().as_secs_f64() * 1000.0);

                let asm_path = output_path.with_extension("asm");
                let lst_path = output_path.with_extension("lst");

                if trial == 0 {
                    let exe = fs::read(&output_path).unwrap();
                    let asm_listing = fs::read_to_string(&asm_path).unwrap();
                    let hex_listing = fs::read_to_string(&lst_path).unwrap();
                    asm_bytes = fs::metadata(&asm_path).map(|m| m.len()).unwrap_or(0);

                    let pe_off = read_u32(&exe, 0x3C) as usize;
                    let import_rva = read_u32(&exe, pe_off + 24 + 112 + 8);
                    let section_off = pe_off + 24 + 240;
                    let virtual_size = read_u32(&exe, section_off + 8) as usize;
                    let raw_ptr = read_u32(&exe, section_off + 20) as usize;
                    let text_bytes = &exe[raw_ptr..raw_ptr + virtual_size];
                    let listing_bytes = parse_hex_listing_bytes(&hex_listing);
                    let import_off = (import_rva - 0x1000) as usize;

                    assert_eq!(&exe[0..2], b"MZ", "{label} {name}");
                    assert_eq!(&exe[pe_off..pe_off + 4], b"PE\0\0", "{label} {name}");
                    assert_eq!(read_u16(&exe, pe_off + 4), 0x8664, "{label} {name}");
                    assert_eq!(read_u16(&exe, pe_off + 24), 0x20B, "{label} {name}");
                    assert!(asm_path.is_file(), "{label} {name}");
                    assert!(lst_path.is_file(), "{label} {name}");
                    assert!(!asm_listing.trim().is_empty(), "{label} {name}");
                    assert!(asm_listing.contains("; === Brainfuck x86_64 Assembly Listing ==="));
                    assert!(hex_listing.contains("; === Brainfuck x86_64 Hex Listing ==="));
                    assert_eq!(listing_bytes[..import_off], text_bytes[..import_off], "{label} {name}");
                    assert!(
                        exe.windows(b"kernel32.dll".len()).any(|w| w == b"kernel32.dll"),
                        "{label} {name}"
                    );
                    pe_len = exe.len();
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
                assert_eq!(
                    runtime_output.stdout, expected,
                    "{label} {name} trial {trial}"
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
                label,
                pe_len,
                asm_bytes,
                c_mean,
                c_min,
                c_max,
                r_mean,
                r_min,
                r_max,
                hir_note,
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
    eprintln!(
        "{:<5} {:>22.3} {:>22.3}",
        "ALL_O", grand_compile, grand_run
    );
}
