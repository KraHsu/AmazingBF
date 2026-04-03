//! # ELF 可执行文件生成器 (elf.rs)
//!
//! 本模块负责将编码后的机器码打包为 ELF64 可执行文件格式。
//!
//! ## ELF 文件结构
//!
//! 生成的 ELF 文件采用最简化的布局：
//!
//! ```text
//! ┌─────────────────────┐  偏移 0x00
//! │    ELF Header       │  64 字节
//! ├─────────────────────┤  偏移 0x40
//! │  Program Header     │  56 字节（1 个 PT_LOAD 段）
//! ├─────────────────────┤  偏移 0x78
//! │    .text            │  机器码（可变长度）
//! └─────────────────────┘
//! ```
//!
//! 特点：
//! - 仅一个 LOAD 段，包含整个文件（ELF 头 + 程序头 + 代码）
//! - 权限为 RWX（读/写/执行），因为 tape 数据和代码在同一段
//! - 无 Section Header Table（意味着 objdump/gdb 无法识别代码段）
//! - 入口点 = 基地址 + ELF 头大小 + 程序头大小 = .text 段起始
//!
//! ## 虚拟地址布局
//!
//! ```text
//! 0x400000               ← 基地址（BASE_VADDR）
//! 0x400000 + 0x78        ← 入口点（.text 起始）
//! 0x400000 + file_size   ← 文件结束
//! ```
//!
//! ## 局限性
//!
//! - 没有 section header，无法被 `objdump -d` 或 GDB 识别代码段
//! - 没有符号表（.symtab），无法显示函数名或标签
//! - 没有调试信息（DWARF），无法做源码级调试
//! - 可以通过 debug.rs 的 hex listing 来弥补这些不足

use crate::backend::x86_64::encode::EncodedProgram;

/// ELF64 文件头大小（固定 64 字节）
const ELF64_EHDR_SIZE: usize = 64;

/// ELF64 程序头大小（固定 56 字节）
const ELF64_PHDR_SIZE: usize = 56;

/// 程序的虚拟地址基地址。
///
/// 0x400000 是 Linux x86_64 上 Position-Dependent Executable 的传统基地址。
/// 内核将 ELF 文件映射到从此地址开始的虚拟内存区域。
const BASE_VADDR: u64 = 0x400000;

/// 段对齐要求。
///
/// 0x1000 = 4096 字节 = 1 页，这是 x86_64 Linux 的页大小。
/// LOAD 段的 p_align 必须是页大小的倍数。
const PAGE_ALIGN: u64 = 0x1000;

/// 将编码后的机器码打包为完整的 ELF64 可执行文件。
///
/// # 参数
/// - `encoded`: 编码后的程序（包含 .text 段的机器码字节）
///
/// # 返回值
/// 完整的 ELF 文件内容，可直接写入磁盘并通过 `chmod +x` 执行
pub fn build_elf_executable(encoded: &EncodedProgram) -> Vec<u8> {
    // .text 段在文件中的偏移 = ELF 头 + 1 个程序头
    let text_off = (ELF64_EHDR_SIZE + ELF64_PHDR_SIZE) as u64;

    // 入口点的虚拟地址 = 基地址 + .text 偏移
    let entry = BASE_VADDR + text_off;

    // 整个文件的大小 = 头部 + 机器码
    let file_size = text_off as usize + encoded.text.len();
    let file_size_u64 = file_size as u64;

    let mut out = Vec::with_capacity(file_size);

    // =====================================================================
    // ELF Header（64 字节）
    // 参考：https://man7.org/linux/man-pages/man5/elf.5.html
    // =====================================================================

    // ---- e_ident[16]: ELF 标识 ----
    out.extend_from_slice(&[
        0x7F, b'E', b'L', b'F', // EI_MAG0~3: ELF 魔数
        2,    // EI_CLASS   = ELFCLASS64（64 位 ELF）
        1,    // EI_DATA    = ELFDATA2LSB（小端序）
        1,    // EI_VERSION = EV_CURRENT（当前版本）
        0,    // EI_OSABI   = ELFOSABI_SYSV（System V ABI）
        0,    // EI_ABIVERSION = 0
        0, 0, 0, 0, 0, 0, 0, // EI_PAD: 填充字节
    ]);

    // ---- e_type: 文件类型 ----
    push_u16(&mut out, 2); // ET_EXEC = 2（可执行文件）

    // ---- e_machine: 目标架构 ----
    push_u16(&mut out, 62); // EM_X86_64 = 62

    // ---- e_version: ELF 版本 ----
    push_u32(&mut out, 1); // EV_CURRENT = 1

    // ---- e_entry: 程序入口点虚拟地址 ----
    push_u64(&mut out, entry);

    // ---- e_phoff: 程序头表在文件中的偏移 ----
    push_u64(&mut out, ELF64_EHDR_SIZE as u64); // 紧跟 ELF 头之后

    // ---- e_shoff: 节头表在文件中的偏移 ----
    push_u64(&mut out, 0); // 无节头表

    // ---- e_flags: 处理器特定标志 ----
    push_u32(&mut out, 0); // x86_64 不使用

    // ---- e_ehsize: ELF 头大小 ----
    push_u16(&mut out, ELF64_EHDR_SIZE as u16);

    // ---- e_phentsize: 程序头表项大小 ----
    push_u16(&mut out, ELF64_PHDR_SIZE as u16);

    // ---- e_phnum: 程序头表项数量 ----
    push_u16(&mut out, 1); // 只有一个 LOAD 段

    // ---- e_shentsize: 节头表项大小 ----
    push_u16(&mut out, 0); // 无节头表

    // ---- e_shnum: 节头表项数量 ----
    push_u16(&mut out, 0); // 无节头表

    // ---- e_shstrndx: 节名字符串表的节头索引 ----
    push_u16(&mut out, 0); // 无节名字符串表

    // =====================================================================
    // Program Header（56 字节）
    // 描述一个可加载段，包含整个文件内容
    // =====================================================================

    // ---- p_type: 段类型 ----
    push_u32(&mut out, 1); // PT_LOAD = 1（可加载段）

    // ---- p_flags: 段权限 ----
    // PF_R(4) | PF_W(2) | PF_X(1) = 7
    // 需要可执行（代码）、可读（常量/跳转目标）、可写（mmap 返回的 tape）
    push_u32(&mut out, 0x4 | 0x2 | 0x1);

    // ---- p_offset: 段在文件中的偏移 ----
    push_u64(&mut out, 0); // 从文件开头开始（包含 ELF 头和程序头）

    // ---- p_vaddr: 段在内存中的虚拟地址 ----
    push_u64(&mut out, BASE_VADDR);

    // ---- p_paddr: 段的物理地址（在现代 OS 中通常等于 p_vaddr） ----
    push_u64(&mut out, BASE_VADDR);

    // ---- p_filesz: 段在文件中的大小 ----
    push_u64(&mut out, file_size_u64);

    // ---- p_memsz: 段在内存中的大小 ----
    // 等于 p_filesz（不需要额外的 BSS 段，因为 tape 通过 mmap 动态分配）
    push_u64(&mut out, file_size_u64);

    // ---- p_align: 段对齐 ----
    push_u64(&mut out, PAGE_ALIGN);

    // ---- 断言：头部大小正确 ----
    assert_eq!(out.len(), ELF64_EHDR_SIZE + ELF64_PHDR_SIZE);

    // =====================================================================
    // .text 段（机器码）
    // =====================================================================
    out.extend_from_slice(&encoded.text);

    out
}

/// 写入 16 位无符号整数（小端序）
fn push_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// 写入 32 位无符号整数（小端序）
fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// 写入 64 位无符号整数（小端序）
fn push_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}
