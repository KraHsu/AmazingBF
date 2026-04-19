//! ELF executable builder (elf.rs).
//!
//! Packages encoded machine code into an ELF64 executable image.
//!
//! ## File layout
//!
//! The emitted ELF uses the minimum layout that Linux will load:
//!
//! ```text
//! ┌─────────────────────┐  offset 0x00
//! │    ELF Header       │  64 bytes
//! ├─────────────────────┤  offset 0x40
//! │  Program Header     │  56 bytes (one PT_LOAD segment)
//! ├─────────────────────┤  offset 0x78
//! │    .text            │  machine code (variable length)
//! └─────────────────────┘
//! ```
//!
//! Properties:
//! - Exactly one LOAD segment, covering the whole file (ELF header, program
//!   header, and code),
//! - Permissions RX; the runtime tape is supplied independently through an
//!   anonymous `mmap` at runtime,
//! - No section header table (so `objdump` / `gdb` cannot identify the code
//!   section),
//! - Entry point = base + ELF header size + program header size = start of
//!   `.text`.
//!
//! ## Virtual-address layout
//!
//! ```text
//! 0x400000               ← base (BASE_VADDR)
//! 0x400000 + 0x78        ← entry point (.text start)
//! 0x400000 + file_size   ← end of image
//! ```
//!
//! ## Known limitations
//!
//! - No section headers, so `objdump -d` and GDB cannot locate code sections,
//! - No symbol table (`.symtab`), so there are no function / label names,
//! - No debug info (DWARF), so source-level debugging is unavailable.
//! - `debug.rs` provides a hex listing that makes up for most of the above.

use crate::backend::x86_64::encode::EncodedProgram;

/// Size of the ELF64 file header (fixed at 64 bytes).
const ELF64_EHDR_SIZE: usize = 64;

/// Size of an ELF64 program header entry (fixed at 56 bytes).
const ELF64_PHDR_SIZE: usize = 56;

/// Virtual-address base for the loaded image.
///
/// 0x400000 is the conventional base for position-dependent executables on
/// Linux x86_64; the kernel maps the ELF file starting here.
const BASE_VADDR: u64 = 0x400000;

/// Segment alignment requirement.
///
/// 0x1000 bytes = 4 KiB = one page on x86_64 Linux. `p_align` for LOAD
/// segments must be a multiple of the page size.
const PAGE_ALIGN: u64 = 0x1000;

/// Build a complete ELF64 executable around the encoded machine code.
///
/// # Parameters
/// - `encoded`: the encoded program carrying `.text` bytes.
///
/// # Returns
/// Full ELF file bytes — ready to write to disk and make executable with
/// `chmod +x`.
pub fn build_elf_executable(encoded: &EncodedProgram) -> Vec<u8> {
    // File offset of .text = ELF header + one program header.
    let text_off = (ELF64_EHDR_SIZE + ELF64_PHDR_SIZE) as u64;

    // Entry-point virtual address = base + .text offset.
    let entry = BASE_VADDR + text_off;

    // Total file size = headers + machine code.
    let file_size = text_off as usize + encoded.text.len();
    let file_size_u64 = file_size as u64;

    let mut out = Vec::with_capacity(file_size);

    // ELF header (64 bytes); see https://man7.org/linux/man-pages/man5/elf.5.html.

    // e_ident[16]: ELF identification.
    out.extend_from_slice(&[
        0x7F, b'E', b'L', b'F', // EI_MAG0..=3: ELF magic
        2,    // EI_CLASS     = ELFCLASS64
        1,    // EI_DATA      = ELFDATA2LSB (little-endian)
        1,    // EI_VERSION   = EV_CURRENT
        0,    // EI_OSABI     = ELFOSABI_SYSV
        0,    // EI_ABIVERSION = 0
        0, 0, 0, 0, 0, 0, 0, // EI_PAD: padding
    ]);

    // e_type: file type.
    push_u16(&mut out, 2); // ET_EXEC = 2 (executable)

    // e_machine: target architecture.
    push_u16(&mut out, 62); // EM_X86_64 = 62

    // e_version: ELF version.
    push_u32(&mut out, 1); // EV_CURRENT = 1

    // e_entry: virtual address of the program entry point.
    push_u64(&mut out, entry);

    // e_phoff: file offset of the program header table.
    push_u64(&mut out, ELF64_EHDR_SIZE as u64); // immediately after the ELF header

    // e_shoff: file offset of the section header table.
    push_u64(&mut out, 0); // no section header table

    // e_flags: processor-specific flags.
    push_u32(&mut out, 0); // unused on x86_64

    // e_ehsize: size of the ELF header.
    push_u16(&mut out, ELF64_EHDR_SIZE as u16);

    // e_phentsize: size of a program header entry.
    push_u16(&mut out, ELF64_PHDR_SIZE as u16);

    // e_phnum: number of program header entries.
    push_u16(&mut out, 1); // a single LOAD segment

    // e_shentsize: size of a section header entry.
    push_u16(&mut out, 0); // no section headers

    // e_shnum: number of section header entries.
    push_u16(&mut out, 0); // no section headers

    // e_shstrndx: section header index of the section-name string table.
    push_u16(&mut out, 0); // no section-name string table

    // Program header (56 bytes): describes one loadable segment that spans the
    // entire file.

    // p_type: segment type.
    push_u32(&mut out, 1); // PT_LOAD = 1 (loadable segment)

    // p_flags: segment permissions.
    // PF_R(4) | PF_X(1) = 5.
    // The code segment does not host the runtime tape; the tape is allocated
    // separately via anonymous mmap.
    push_u32(&mut out, 0x4 | 0x1);

    // p_offset: file offset of the segment.
    push_u64(&mut out, 0); // starts at file offset 0 (covers headers + code)

    // p_vaddr: virtual address of the segment.
    push_u64(&mut out, BASE_VADDR);

    // p_paddr: physical address (on modern OSes usually equal to p_vaddr).
    push_u64(&mut out, BASE_VADDR);

    // p_filesz: segment size in the file.
    push_u64(&mut out, file_size_u64);

    // p_memsz: segment size in memory.
    // Same as p_filesz (no extra BSS needed; the tape is mmap'd at runtime).
    push_u64(&mut out, file_size_u64);

    // p_align: segment alignment.
    push_u64(&mut out, PAGE_ALIGN);

    // Sanity check: the header block has exactly the expected size.
    assert_eq!(out.len(), ELF64_EHDR_SIZE + ELF64_PHDR_SIZE);

    // .text segment (machine code).
    out.extend_from_slice(&encoded.text);

    out
}

/// Append a little-endian `u16`.
fn push_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Append a little-endian `u32`.
fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Append a little-endian `u64`.
fn push_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::x86_64::encode::EncodedProgram;

    fn read_u16(bytes: &[u8], offset: usize) -> u16 {
        u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
    }

    fn read_u32(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    fn read_u64(bytes: &[u8], offset: usize) -> u64 {
        u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
    }

    #[test]
    fn elf_header_and_segment_flags_match_backend_contract() {
        let encoded = EncodedProgram {
            text: vec![0xC3], // ret
        };
        let elf = build_elf_executable(&encoded);

        assert_eq!(&elf[0..4], b"\x7FELF");
        assert_eq!(read_u16(&elf, 16), 2);
        assert_eq!(read_u16(&elf, 18), 62);
        assert_eq!(
            read_u64(&elf, 24),
            BASE_VADDR + (ELF64_EHDR_SIZE + ELF64_PHDR_SIZE) as u64
        );
        assert_eq!(read_u64(&elf, 32), ELF64_EHDR_SIZE as u64);
        assert_eq!(read_u16(&elf, 54), ELF64_PHDR_SIZE as u16);
        assert_eq!(read_u32(&elf, ELF64_EHDR_SIZE), 1);
        assert_eq!(read_u32(&elf, ELF64_EHDR_SIZE + 4), 0x5);
        assert_eq!(read_u64(&elf, ELF64_EHDR_SIZE + 32), elf.len() as u64);
        assert_eq!(read_u64(&elf, ELF64_EHDR_SIZE + 40), elf.len() as u64);
    }
}
