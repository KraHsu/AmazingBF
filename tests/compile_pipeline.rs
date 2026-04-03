use assert_cmd::assert::OutputAssertExt;
use assert_cmd::cargo::CommandCargoExt;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
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

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
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

#[test]
fn compile_mode_emits_rx_elf_artifacts_and_preserves_eof_semantics() {
    let temp = TempDirGuard::new("amazingbf-compile");
    let source_path = temp.path().join("eof.bf");
    let output_path = temp.path().join("eof_program");

    fs::write(&source_path, ",.").unwrap();

    Command::cargo_bin("AmazingBF")
        .unwrap()
        .arg(&source_path)
        .arg("-m")
        .arg("compile")
        .arg("-o")
        .arg(&output_path)
        .assert()
        .success();

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

    assert_eq!(&elf[0..4], b"\x7FELF");
    assert_eq!(read_u32(&elf, 64), 1);
    assert_eq!(read_u32(&elf, 68), 0x5);
    assert!(metadata.permissions().mode() & 0o111 != 0);
    assert!(asm_path.is_file());
    assert!(lst_path.is_file());
    assert!(!asm_listing.trim().is_empty());
    assert!(asm_listing.contains("; === Brainfuck x86_64 Assembly Listing ==="));
    assert!(hex_listing.contains("; === Brainfuck x86_64 Hex Listing ==="));
    assert_eq!(parse_hex_listing_bytes(&hex_listing), elf_text);

    let output = Command::new(&output_path).output().unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, vec![255]);
}
