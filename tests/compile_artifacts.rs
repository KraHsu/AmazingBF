//! Artifact correctness for `compile` mode over `tests/cases/*.bf`:
//! each case × `-O0..3` is compiled once, the ELF/PE binary and its
//! `.asm` / `.lst` debug artifacts are validated, and the binary is run
//! against the fixture stdin (if any) to check its stdout matches the
//! expected `.out`.
//!
//! Timing / RSS / aggregation used to live here; it now lives in
//! `benches/compile_levels.rs`.

mod common;

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
#[cfg(target_os = "windows")]
use std::thread;
#[cfg(target_os = "windows")]
use std::time::Duration;
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

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

#[allow(dead_code)]
fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

#[cfg(not(target_os = "windows"))]
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

/// Large PE writes right after heavy compiler work occasionally fail on Windows (AV indexing,
/// transient `ERROR_SHARING_VIOLATION`). One short retry keeps the test from flaking.
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
#[test]
fn compile_mode_emits_rx_elf_artifacts_and_preserves_eof_semantics() {
    let levels: &[(&str, &str)] = &[("0", "O0"), ("1", "O1"), ("2", "O2"), ("3", "O3")];

    let temp = TempDirGuard::new("amazingbf-compile");
    let amazingbf = Path::new(env!("CARGO_BIN_EXE_AmazingBF"));

    for bf_file in case_paths() {
        let name = bf_file.file_stem().unwrap().to_string_lossy().into_owned();
        let in_file = Path::new(CASES_DIR).join(format!("{name}.in"));
        let out_file = Path::new(CASES_DIR).join(format!("{name}.out"));
        assert!(
            out_file.is_file(),
            "[compile_artifacts] {name}: missing .out"
        );
        let expected = common::read_fixture_bytes(&out_file);

        for (flag, label) in levels {
            let output_path = temp.path().join(format!("{name}_eof_{}", flag));

            assert_amazingbf_compile_ok(amazingbf, &bf_file, flag, &output_path, &name);

            let asm_path = output_path.with_extension("asm");
            let lst_path = output_path.with_extension("lst");

            let elf = fs::read(&output_path).unwrap();
            let asm_listing = fs::read_to_string(&asm_path).unwrap();
            let hex_listing = fs::read_to_string(&lst_path).unwrap();
            let metadata = fs::metadata(&output_path).unwrap();
            let entry = read_u64(&elf, 24);
            let base_vaddr = read_u64(&elf, 80);
            let text_offset = usize::try_from(entry - base_vaddr).unwrap();
            let elf_text = &elf[text_offset..];

            assert_eq!(&elf[0..4], b"\x7FELF", "{label} {name}");
            assert_eq!(read_u32(&elf, 64), 1, "{label} {name}");
            assert_eq!(read_u32(&elf, 68), 0x5, "{label} {name}");
            assert!(metadata.permissions().mode() & 0o111 != 0, "{label} {name}");
            assert!(asm_path.is_file(), "{label} {name}");
            assert!(lst_path.is_file(), "{label} {name}");
            assert!(!asm_listing.trim().is_empty(), "{label} {name}");
            assert!(asm_listing.contains("; === Brainfuck x86_64 Assembly Listing ==="));
            assert!(hex_listing.contains("; === Brainfuck x86_64 Hex Listing ==="));
            assert_eq!(
                parse_hex_listing_bytes(&hex_listing),
                elf_text,
                "{label} {name}"
            );

            let runtime_output =
                common::run_with_optional_input(Command::new(&output_path), &in_file);

            assert!(
                runtime_output.status.success(),
                "{label} {name} stderr:\n{}",
                String::from_utf8_lossy(&runtime_output.stderr)
            );
            assert_eq!(runtime_output.stdout, expected, "{label} {name}");
        }
    }
}

#[cfg(target_os = "windows")]
#[test]
fn compile_mode_emits_rx_pe_artifacts_and_preserves_eof_semantics() {
    let levels: &[(&str, &str)] = &[("0", "O0"), ("1", "O1"), ("2", "O2"), ("3", "O3")];

    let temp = TempDirGuard::new("amazingbf-compile");
    let amazingbf = Path::new(env!("CARGO_BIN_EXE_AmazingBF"));

    for bf_file in case_paths() {
        let name = bf_file.file_stem().unwrap().to_string_lossy().into_owned();
        let in_file = Path::new(CASES_DIR).join(format!("{name}.in"));
        let out_file = Path::new(CASES_DIR).join(format!("{name}.out"));
        assert!(
            out_file.is_file(),
            "[compile_artifacts] {name}: missing .out"
        );
        let expected = common::read_fixture_bytes(&out_file);

        for (flag, label) in levels {
            let output_path = temp.path().join(format!("{name}_eof_{}.exe", flag));

            assert_amazingbf_compile_ok(amazingbf, &bf_file, flag, &output_path, &name);

            let asm_path = output_path.with_extension("asm");
            let lst_path = output_path.with_extension("lst");

            let exe = fs::read(&output_path).unwrap();
            let asm_listing = fs::read_to_string(&asm_path).unwrap();
            let hex_listing = fs::read_to_string(&lst_path).unwrap();

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
            assert_eq!(
                listing_bytes[..import_off],
                text_bytes[..import_off],
                "{label} {name}"
            );
            assert!(
                exe.windows(b"kernel32.dll".len())
                    .any(|w| w == b"kernel32.dll"),
                "{label} {name}"
            );

            let runtime_output =
                common::run_with_optional_input(Command::new(&output_path), &in_file);

            assert!(
                runtime_output.status.success(),
                "{label} {name} stderr:\n{}",
                String::from_utf8_lossy(&runtime_output.stderr)
            );
            assert_eq!(runtime_output.stdout, expected, "{label} {name}");
        }
    }
}
