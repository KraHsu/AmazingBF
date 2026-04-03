//! # x86_64 后端模块 (x86_64/mod.rs)
//!
//! 本模块是 x86_64 目标平台的入口，包含：
//! - `elf`: ELF64 可执行文件生成器
//! - `encode`: x86_64 机器码编码器
//! - `debug`: 调试输出工具（汇编 listing + hex dump）
//!
//! 提供 `compile_asm_to_elf` 作为后端落地入口，
//! 将汇编 IR 编译到可执行的 ELF 二进制文件。

pub mod elf;
pub mod encode;

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
/// std::fs::write("output.asm", debug::dump_asm_listing(&asm))?;
///
/// // 输出 hex listing 到文件
/// std::fs::write("output.lst", debug::dump_hex_listing(&asm))?;
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
