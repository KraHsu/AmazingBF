//! # x86_64 后端模块 (x86_64/mod.rs)
//!
//! 本模块是 x86_64 目标平台的入口，包含：
//! - `elf`: ELF64 可执行文件生成器
//! - `encode`: x86_64 机器码编码器
//! - `debug`: 调试输出工具（汇编 listing + hex dump）
//! - `pe`: PE32+ 可执行文件生成器
//! - `windows`: x86_64 Windows 代码生成辅助
//!
//! 提供 `compile_asm_to_elf` 作为后端落地入口，
//! 将汇编 IR 编译到目标平台可执行文件。

pub mod elf;
pub mod encode;
pub mod pe;
pub mod windows;

/// 调试输出模块。
///
/// 提供两种调试输出格式：
/// - `dump_asm_listing`: 人类可读的汇编文本（方案 A）
/// - `dump_hex_listing`: 带偏移量的十六进制字节转储（方案 B）
///
/// 使用示例：
/// ```rust,ignore
/// let asm = compile_lir_to_asm(&lir);
///
/// // 输出汇编文本到文件
/// std::fs::write("hello_bf.asm", debug::dump_asm_listing(&asm))?;
///
/// // 输出 hex listing 到文件
/// std::fs::write("hello_bf.lst", debug::dump_hex_listing(&asm))?;
/// ```
pub mod debug;

use crate::backend::asm::AsmProgram;
use tracing::debug;

/// 将汇编 IR 编译为 ELF64 可执行文件。
///
/// 这是 x86_64 后端的顶层入口，串联了编译流水线的最后几步：
///
/// ```text
/// AsmProgram
///   → encode_program       (encode.rs)    → EncodedProgram
///   → build_elf_executable (elf.rs)       → Vec<u8>（ELF 文件内容）
/// ```
///
/// # 参数
/// - `asm`: 要编码的汇编程序
///
/// # 返回值
/// 完整的 ELF 文件内容，可写入磁盘并执行
pub fn compile_asm_to_elf(asm: &AsmProgram) -> Vec<u8> {
    let encoded = encode::encode_program(asm);
    debug!(
        target: "AmazingBF::backend::x86_64",
        asm_insts = asm.insts.len(),
        text_bytes = encoded.text.len(),
        "encoded x86_64 machine code"
    );
    elf::build_elf_executable(&encoded)
}

pub fn compile_windows_program_to_pe(program: &windows::WindowsProgram) -> Vec<u8> {
    let (mut encoded, labels) = encode::encode_program_with_labels(&program.asm);
    debug!(
        target: "AmazingBF::backend::x86_64",
        asm_insts = program.asm.insts.len(),
        text_bytes = encoded.text.len(),
        "encoded x86_64 windows machine code"
    );

    let label_offset = |label| -> usize {
        *labels
            .get(&label)
            .unwrap_or_else(|| panic!("missing encoded offset for label {:?}", label))
    };
    let label_rva = |label| -> u32 {
        0x1000 + u32::try_from(label_offset(label)).expect("label offset exceeded u32")
    };

    patch_u32(&mut encoded.text, label_offset(program.import_desc_label), label_rva(program.imports[0].ilt_entry_label));
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
