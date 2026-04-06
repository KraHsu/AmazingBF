//! Windows-targeted regression tests for PE64 output and cross-target compiler behavior.

use assert_cmd::cargo::CommandCargoExt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
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

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
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

#[test]
fn compile_windows_target_emits_pe_artifacts_and_imports_kernel32() {
    let temp = TempDirGuard::new("amazingbf-windows");
    let output_path = temp.path().join("hello.exe");
    let asm_path = output_path.with_extension("asm");
    let lst_path = output_path.with_extension("lst");
    let source_path = Path::new(CASES_DIR).join("1.bf");

    let status = Command::cargo_bin("AmazingBF")
        .unwrap()
        .arg("-q")
        .arg(&source_path)
        .arg("-m")
        .arg("compile")
        // Default -O3 folds stdin-less programs to a tiny import set (no VirtualAlloc); use -O0
        // so this test exercises the full Windows LIR backend and kernel32 import table.
        .arg("-O0")
        .arg("--target")
        .arg("x86_64-windows")
        .arg("-o")
        .arg(&output_path)
        .status()
        .unwrap();
    assert!(status.success());

    let exe = fs::read(&output_path).unwrap();
    let asm_listing = fs::read_to_string(&asm_path).unwrap();
    let hex_listing = fs::read_to_string(&lst_path).unwrap();

    assert_eq!(&exe[0..2], b"MZ");
    let pe_off = read_u32(&exe, 0x3C) as usize;
    assert_eq!(&exe[pe_off..pe_off + 4], b"PE\0\0");
    assert_eq!(read_u16(&exe, pe_off + 4), 0x8664);
    assert_eq!(read_u16(&exe, pe_off + 24), 0x20B);

    let import_rva = read_u32(&exe, pe_off + 24 + 112 + 8);
    let import_size = read_u32(&exe, pe_off + 24 + 112 + 12);
    let iat_rva = read_u32(&exe, pe_off + 24 + 112 + 12 * 8);
    let iat_size = read_u32(&exe, pe_off + 24 + 112 + 12 * 8 + 4);
    assert!(import_rva >= 0x1000);
    assert_eq!(import_size, 40);
    assert!(iat_rva >= 0x1000);
    assert!(iat_size >= 8);

    let section_off = pe_off + 24 + 240;
    let virtual_size = read_u32(&exe, section_off + 8) as usize;
    let raw_ptr = read_u32(&exe, section_off + 20) as usize;
    let text_bytes = &exe[raw_ptr..raw_ptr + virtual_size];
    let listing_bytes = parse_hex_listing_bytes(&hex_listing);
    let import_off = (import_rva - 0x1000) as usize;
    assert_eq!(listing_bytes[..import_off], text_bytes[..import_off]);

    assert!(asm_listing.contains("call    qword [rel"));
    assert!(
        asm_listing.contains("ExitProcess")
            || exe
                .windows(b"ExitProcess".len())
                .any(|w| w == b"ExitProcess")
    );
    assert!(
        exe.windows(b"kernel32.dll".len())
            .any(|w| w == b"kernel32.dll")
    );
    assert!(
        exe.windows(b"GetStdHandle".len())
            .any(|w| w == b"GetStdHandle")
    );
    assert!(exe.windows(b"WriteFile".len()).any(|w| w == b"WriteFile"));
    assert!(
        exe.windows(b"VirtualAlloc".len())
            .any(|w| w == b"VirtualAlloc")
    );
}

#[test]
fn bf_compiler_defaults_to_build_target_and_supports_cross_compile() {
    let temp = TempDirGuard::new("amazingbf-cross-target");
    let source_path = Path::new(CASES_DIR).join("1.bf");
    let default_output = temp.path().join("default_target.bin");
    let cross_output = temp.path().join("cross_target.bin");

    let default_status = Command::cargo_bin("bf-compiler")
        .unwrap()
        .arg("-q")
        .arg(&source_path)
        .arg("-o")
        .arg(&default_output)
        .status()
        .unwrap();
    assert!(default_status.success());

    let cross_target = if cfg!(target_os = "windows") {
        "x86_64-linux"
    } else {
        "x86_64-windows"
    };
    let cross_status = Command::cargo_bin("bf-compiler")
        .unwrap()
        .arg("-q")
        .arg(&source_path)
        .arg("--target")
        .arg(cross_target)
        .arg("-o")
        .arg(&cross_output)
        .status()
        .unwrap();
    assert!(cross_status.success());

    let default_bytes = fs::read(&default_output).unwrap();
    let cross_bytes = fs::read(&cross_output).unwrap();

    if cfg!(target_os = "windows") {
        assert_eq!(&default_bytes[0..2], b"MZ");
        assert_eq!(&cross_bytes[0..4], b"\x7FELF");
    } else {
        assert_eq!(&default_bytes[0..4], b"\x7FELF");
        assert_eq!(&cross_bytes[0..2], b"MZ");
    }
}
