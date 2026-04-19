//! x86_64 native backend entry points.
//!
//! `compile` mode reaches this module after HIR has been lowered to LIR and then to
//! backend assembly IR. The Linux path packages machine code as ELF64, while the
//! Windows path packages machine code plus import tables as PE32+.

pub(crate) mod elf;
pub(crate) mod encode;
pub(crate) mod pe;
pub(crate) mod windows;

/// Debug-output module.
///
/// Provides two listing formats:
/// - `dump_asm_listing`: human-readable assembly text (scheme A).
/// - `dump_hex_listing`: per-instruction hex byte dump with offsets (scheme B).
///
/// Usage:
/// ```rust,ignore
/// let asm = compile_lir_to_asm(&lir);
///
/// // Write human-readable assembly text.
/// std::fs::write("hello_bf.asm", debug::dump_asm_listing(&asm))?;
///
/// // Write the hex listing.
/// std::fs::write("hello_bf.lst", debug::dump_hex_listing(&asm))?;
/// ```
pub(crate) mod debug;

use crate::backend::asm::AsmProgram;
use crate::logging::log_debug;

/// Compile backend assembly IR into a Linux ELF64 executable.
///
/// This is the Linux x86_64 backend entry point for the final stages:
///
/// ```text
/// AsmProgram
///   → encode_program       (encode.rs)    → EncodedProgram
///   → build_elf_executable (elf.rs)       → Vec<u8>  (ELF file bytes)
/// ```
///
/// # Parameters
/// - `asm`: assembly program to encode
///
/// # Returns
/// Full ELF file bytes ready to write to disk.
pub fn compile_asm_to_elf(asm: &AsmProgram) -> Vec<u8> {
    let encoded = encode::encode_program(asm);
    log_debug(format!(
        "encoded x86_64 machine code (asm_insts={} text_bytes={})",
        asm.insts.len(),
        encoded.text.len()
    ));
    elf::build_elf_executable(&encoded)
}

/// Compile a Windows-specific backend program into a PE32+ executable.
pub fn compile_windows_program_to_pe(program: &windows::WindowsProgram) -> Vec<u8> {
    let (mut encoded, labels) = encode::encode_program_with_labels(&program.asm);
    log_debug(format!(
        "encoded x86_64 windows machine code (asm_insts={} text_bytes={})",
        program.asm.insts.len(),
        encoded.text.len()
    ));

    let label_offset = |label| -> usize {
        *labels
            .get(&label)
            .unwrap_or_else(|| panic!("missing encoded offset for label {:?}", label))
    };
    let label_rva = |label| -> u32 {
        0x1000 + u32::try_from(label_offset(label)).expect("label offset exceeded u32")
    };

    patch_u32(
        &mut encoded.text,
        label_offset(program.import_desc_label),
        label_rva(program.imports[0].ilt_entry_label),
    );
    patch_u32(
        &mut encoded.text,
        label_offset(program.import_desc_label) + 12,
        label_rva(program.dll_name_label),
    );
    patch_u32(
        &mut encoded.text,
        label_offset(program.import_desc_label) + 16,
        label_rva(program.imports[0].iat_entry_label),
    );

    for import in &program.imports {
        debug_assert!(!import.name.is_empty());
        patch_u64(
            &mut encoded.text,
            label_offset(import.ilt_entry_label),
            u64::from(label_rva(import.hint_name_label)),
        );
        patch_u64(
            &mut encoded.text,
            label_offset(import.iat_entry_label),
            u64::from(label_rva(import.hint_name_label)),
        );
    }

    pe::build_pe_executable(
        &encoded,
        u32::try_from(label_offset(program.entry_label)).expect("entry offset exceeded u32"),
        pe::DataDirectory {
            rva: label_rva(program.import_desc_label),
            size: program.import_dir_size,
        },
        pe::DataDirectory {
            rva: label_rva(program.iat_label),
            size: program.iat_size,
        },
    )
}

fn patch_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn patch_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
