//! # LIR → x86_64 汇编代码生成器 (codegen.rs)
//!
//! 本模块负责将低级中间表示（LIR）翻译为 x86_64 汇编指令序列（`AsmProgram`）。
//!
//! ## 编译策略
//!
//! Brainfuck 虚拟机的核心状态是一条无限长的字节数组（"tape"）和一个指针。
//! 本编译器使用 mmap 分配 tape，并在指针超出范围时动态扩容。
//!
//! ### 寄存器分配
//!
//! 编译器使用固定的寄存器分配方案（不需要寄存器分配器）：
//!
//! | 寄存器 | 角色              | 说明                                |
//! |--------|-------------------|-------------------------------------|
//! | R12    | tape_base         | mmap 返回的缓冲区起始地址           |
//! | R13    | data_ptr          | 当前 BF 指针位置（BF 的 ">"/"<"）   |
//! | R14    | tape_end          | 缓冲区结束地址 = base + length      |
//! | R15    | scratch           | PtrAdd 时的候选新指针（边界检查前）  |
//!
//! ### Tape 扩容策略
//!
//! 当 `PtrAdd` 导致指针越界时（< base 或 >= end），调用 `ensure_tape_contains_r15`：
//! 1. 反复将 tape 长度翻倍，直到新 tape 能容纳目标地址
//! 2. mmap 分配新缓冲区
//! 3. 将旧数据居中复制到新缓冲区
//! 4. munmap 释放旧缓冲区
//! 5. 更新 R12/R13/R14 三个寄存器
//!
//! ## 标签分配
//!
//! - 用户标签：直接从 LIR 的 `LabelId` 映射（低位 u32 值）
//! - 内部标签：从 `u32::MAX` 递减分配，避免与用户标签冲突

use crate::backend::asm::{AsmInst, AsmLabel, AsmProgram, Reg64};
use crate::ir::lir::{LabelId, LirInst, LirProgram};

/// 初始 tape 大小（字节）。
///
/// 4096 = 1 页，足以运行大多数简单的 BF 程序。
/// 如果程序需要更多空间，会通过 ensure_tape 动态扩容。
const INITIAL_TAPE_SIZE: usize = 4096;

/// 内部标签 ID：`ensure_tape` 函数的入口点。
///
/// 当 PtrAdd 检测到指针越界时，会 CALL 到此标签。
const INTERNAL_LABEL_ENSURE_TAPE_RAW: u32 = u32::MAX;

/// 内部标签 ID：统一的失败退出点。
///
/// 当 mmap/read 等系统调用返回负值（失败）时，跳转到此标签执行 exit(1)。
const INTERNAL_LABEL_EXIT_ONE_RAW: u32 = u32::MAX - 1;

/// 内部标签 ID：`ensure_tape` 中翻倍循环的入口。
///
/// 这个标签用于 tape 扩容时的"反复翻倍直到足够大"的循环。
const INTERNAL_LABEL_GROW_LOOP_RAW: u32 = u32::MAX - 2;

/// 保留内部标签区间的最低值（含）。
///
/// [INTERNAL_LABEL_RESERVED_MIN_RAW, u32::MAX] 这段编号空间
/// 专门留给“固定语义”的内部标签，不能被 fresh_internal_label() 占用。
const INTERNAL_LABEL_RESERVED_MIN_RAW: u32 = INTERNAL_LABEL_GROW_LOOP_RAW;

/// 临时内部标签 ID 的分配起点。
///
/// PtrAdd 的边界检查需要生成临时标签（slow_path 和 done），
/// 它们从“保留内部标签区间”以下开始递减分配，避免与固定内部标签撞号。
const INTERNAL_LABEL_BASE_RAW: u32 = INTERNAL_LABEL_RESERVED_MIN_RAW - 1;

/// 将 LIR 程序编译为 x86_64 汇编程序。
///
/// 生成的程序结构如下：
/// ```text
/// [初始化 tape]       ← emit_init_tape
/// [翻译后的 BF 指令]  ← 主循环
/// [exit(0)]            ← 正常退出
/// [ensure_tape 函数]   ← emit_ensure_tape_contains_r15
/// [exit(1)]            ← emit_exit_one（OOM 退出）
/// ```
///
/// # 参数
/// - `lir`: 要编译的 LIR 程序
///
/// # 返回值
/// 编译后的汇编程序
pub fn compile_lir_to_asm(lir: &LirProgram) -> AsmProgram {
    let ensure_tape_label = AsmLabel(INTERNAL_LABEL_ENSURE_TAPE_RAW);
    let exit_one_label = AsmLabel(INTERNAL_LABEL_EXIT_ONE_RAW);

    // 内部标签计数器，从 INTERNAL_LABEL_BASE_RAW 递减分配
    let mut next_internal_label = INTERNAL_LABEL_BASE_RAW;

    let mut out = Vec::new();

    // ==== 1. 初始化 tape（mmap 分配内存） ====
    emit_init_tape(&mut out, exit_one_label);

    // ==== 2. 翻译 LIR 指令 ====
    for inst in &lir.insts {
        match inst {
            // ---- PtrAdd(0)：空操作，跳过 ----
            LirInst::PtrAdd(0) => {}

            // ---- PtrAdd(n)：移动数据指针 ----
            //
            // 生成的代码流程：
            //   r15 = r13 + n          （计算候选新位置）
            //   if r15 < r12: goto slow_path   （低于 tape 起始）
            //   if r15 >= r14: goto slow_path   （超过 tape 末尾）
            //   r13 = r15              （快速路径：直接更新指针）
            //   goto done
            // slow_path:
            //   call ensure_tape       （慢速路径：扩容后更新指针）
            // done:
            LirInst::PtrAdd(n) => {
                // 分配两个临时标签用于边界检查的跳转
                let slow_path = fresh_internal_label(&mut next_internal_label);
                let done = fresh_internal_label(&mut next_internal_label);

                // 快速路径：先计算目标地址，检查是否在 [base, end) 范围内
                out.push(AsmInst::MovRegReg(Reg64::R15, Reg64::R13)); // r15 = current_ptr
                emit_add_reg_isize(&mut out, Reg64::R15, *n); // r15 += offset

                // 无符号比较：r15 < tape_base？
                out.push(AsmInst::CmpRegReg(Reg64::R15, Reg64::R12));
                out.push(AsmInst::Jb(slow_path));

                // 无符号比较：r15 >= tape_end？
                out.push(AsmInst::CmpRegReg(Reg64::R15, Reg64::R14));
                out.push(AsmInst::Jae(slow_path));

                // 快速路径：指针在有效范围内，直接更新
                out.push(AsmInst::MovRegReg(Reg64::R13, Reg64::R15));
                out.push(AsmInst::Jmp(done));

                // 慢速路径：需要扩容
                out.push(AsmInst::Label(slow_path));
                out.push(AsmInst::Call(ensure_tape_label));
                // ensure_tape 返回后，R12/R13/R14 已经被更新为新 tape 的值

                out.push(AsmInst::Label(done));
            }

            // ---- CellAdd(0)：空操作，跳过 ----
            LirInst::CellAdd(0) => {}

            // ---- CellAdd(n)：修改当前单元的值 ----
            //
            // BF 的 '+' 和 '-' 经过合并后变成 CellAdd(n)，
            // 其中 n 可以是负数（对应 '-'）。
            //
            // 由于 BF 的单元是 8 位无符号整数（0~255），
            // 我们只需要取 n mod 256 的结果作为加数。
            LirInst::CellAdd(n) => {
                // 将 n 映射到 0..=255 范围
                // (n % 256 + 256) % 256 确保负数也能正确映射
                // 例如：-1 → 255，-3 → 253
                let imm = ((*n % 256) + 256) % 256;
                if imm != 0 {
                    // 等价于 `*data_ptr = (*data_ptr + imm) % 256`
                    out.push(AsmInst::AddMem8Imm8(Reg64::R13, imm as u8 as i8));
                }
            }

            // ---- CellSet(v)：将当前单元设置为指定值 ----
            //
            // 这是一个优化指令，由 `[-]` 或 `[+]` 等模式识别生成。
            // 直接将内存字节设为 v，无需先读取再修改。
            LirInst::CellSet(v) => {
                out.push(AsmInst::MovMem8Imm8(Reg64::R13, *v));
            }

            // ---- PutByte：输出当前单元 ----
            //
            // 等价于 BF 的 '.' 操作。
            // 使用 Linux sys_write 系统调用：
            //   write(fd=1(stdout), buf=data_ptr, count=1)
            LirInst::PutByte => {
                out.push(AsmInst::MovRegImm64(Reg64::Rax, 1)); // syscall 号 = 1 (write)
                out.push(AsmInst::MovRegImm64(Reg64::Rdi, 1)); // fd = 1 (stdout)
                out.push(AsmInst::MovRegReg(Reg64::Rsi, Reg64::R13)); // buf = data_ptr
                out.push(AsmInst::MovRegImm64(Reg64::Rdx, 1)); // count = 1
                out.push(AsmInst::Syscall);
            }

            // ---- GetByte：读取一个字节到当前单元 ----
            //
            // 等价于 BF 的 ',' 操作。
            // 使用 Linux sys_read 系统调用：
            //   read(fd=0(stdin), buf=data_ptr, count=1)
            //
            // 语义上需要与解释器保持一致：
            // - 返回 1：成功读取一个字节，内核已经写入 *data_ptr
            // - 返回 0：EOF，将当前单元设为 255
            // - 返回负值：读取失败，exit(1)
            LirInst::GetByte => {
                let done = fresh_internal_label(&mut next_internal_label);
                out.push(AsmInst::MovRegImm64(Reg64::Rax, 0)); // syscall 号 = 0 (read)
                out.push(AsmInst::MovRegImm64(Reg64::Rdi, 0)); // fd = 0 (stdin)
                out.push(AsmInst::MovRegReg(Reg64::Rsi, Reg64::R13)); // buf = data_ptr
                out.push(AsmInst::MovRegImm64(Reg64::Rdx, 1)); // count = 1
                out.push(AsmInst::Syscall);
                out.push(AsmInst::CmpRegImm32(Reg64::Rax, 0));
                out.push(AsmInst::Jl(exit_one_label));
                out.push(AsmInst::Jnz(done));
                out.push(AsmInst::MovMem8Imm8(Reg64::R13, 255));
                out.push(AsmInst::Label(done));
            }

            // ---- Label：标签定义 ----
            //
            // 直接从 LIR 的 LabelId 映射到 AsmLabel。
            LirInst::Label(id) => {
                out.push(AsmInst::Label(map_label(*id)));
            }

            // ---- JumpIfZero：当前单元为零时跳转 ----
            //
            // 对应 BF 的 '['：如果当前单元为 0，跳过循环体。
            LirInst::JumpIfZero(id) => {
                out.push(AsmInst::CmpMem8Imm8(Reg64::R13, 0)); // 比较 *data_ptr 与 0
                out.push(AsmInst::Jz(map_label(*id))); // 如果为零则跳转
            }

            // ---- JumpIfNonZero：当前单元非零时跳转 ----
            //
            // 对应 BF 的 ']'：如果当前单元不为 0，跳回循环开头。
            LirInst::JumpIfNonZero(id) => {
                out.push(AsmInst::CmpMem8Imm8(Reg64::R13, 0)); // 比较 *data_ptr 与 0
                out.push(AsmInst::Jnz(map_label(*id))); // 如果非零则跳转
            }

            // 兜底分支：如果 LIR 增加了新指令但后端未实现
            #[allow(unreachable_patterns)]
            _ => {
                panic!("unsupported LIR instruction in backend");
            }
        }
    }

    // ==== 3. 程序正常退出：exit(0) ====
    out.push(AsmInst::MovRegImm64(Reg64::Rax, 60)); // syscall 号 = 60 (exit)
    out.push(AsmInst::MovRegImm64(Reg64::Rdi, 0)); // 退出码 = 0
    out.push(AsmInst::Syscall);

    // ==== 4. 辅助函数 ====
    emit_ensure_tape_contains_r15(&mut out, ensure_tape_label, exit_one_label);
    emit_exit_one(&mut out, exit_one_label);

    AsmProgram { insts: out }
}

/// 最小 ELF 载荷：仅 `exit(0)`。用于 `-O3` 且源码中无任何 `.` 时。
pub fn compile_trivial_exit_asm() -> AsmProgram {
    AsmProgram {
        insts: vec![
            AsmInst::MovRegImm64(Reg64::Rax, 60),
            AsmInst::MovRegImm64(Reg64::Rdi, 0),
            AsmInst::Syscall,
        ],
    }
}

/// `write(1, buf, n); exit(0)`，字节放在栈上，不经过 Brainfuck tape。
///
/// `data` 为空时等价于 [`compile_trivial_exit_asm`]。
pub fn compile_precomputed_stdout_asm(data: &[u8]) -> AsmProgram {
    if data.is_empty() {
        return compile_trivial_exit_asm();
    }
    AsmProgram {
        insts: vec![AsmInst::RawBytes(build_precomputed_stdout_machine_code(data))],
    }
}

/// 生成 `sub rsp` / `mov [rsp+off]` / `write` / `exit` 的机器码（Linux x86_64）。
fn build_precomputed_stdout_machine_code(data: &[u8]) -> Vec<u8> {
    let n = data.len();
    let mut code = Vec::new();
    let alloc = (n + 15) & !15;

    emit_sub_rsp_imm32(&mut code, alloc as i32);

    for (i, &b) in data.iter().enumerate() {
        emit_mov_byte_rsp_offset(&mut code, i, b);
    }

    emit_mov_reg_imm64_low3(&mut code, 0, 1);
    emit_mov_reg_imm64_low3(&mut code, 7, 1);
    code.extend_from_slice(&[0x48, 0x89, 0xe6]);
    emit_mov_reg_imm64_low3(&mut code, 2, n as i64);

    code.extend_from_slice(&[0x0f, 0x05]);

    emit_add_rsp_imm32(&mut code, alloc as i32);

    emit_mov_reg_imm64_low3(&mut code, 0, 60);
    emit_mov_reg_imm64_low3(&mut code, 7, 0);
    code.extend_from_slice(&[0x0f, 0x05]);

    code
}

fn emit_mov_reg_imm64_low3(code: &mut Vec<u8>, reg_low3: u8, imm: i64) {
    debug_assert!(reg_low3 < 8);
    code.push(0x48);
    code.push(0xb8 + reg_low3);
    code.extend_from_slice(&imm.to_le_bytes());
}

fn emit_sub_rsp_imm32(code: &mut Vec<u8>, imm: i32) {
    debug_assert!(imm > 0);
    if imm <= 127 {
        code.extend_from_slice(&[0x48, 0x83, 0xec, imm as u8]);
    } else {
        code.push(0x48);
        code.push(0x81);
        code.push(0xec);
        code.extend_from_slice(&imm.to_le_bytes());
    }
}

fn emit_add_rsp_imm32(code: &mut Vec<u8>, imm: i32) {
    debug_assert!(imm > 0);
    if imm <= 127 {
        code.extend_from_slice(&[0x48, 0x83, 0xc4, imm as u8]);
    } else {
        code.push(0x48);
        code.push(0x81);
        code.push(0xc4);
        code.extend_from_slice(&imm.to_le_bytes());
    }
}

fn emit_mov_byte_rsp_offset(code: &mut Vec<u8>, offset: usize, val: u8) {
    if offset <= 127 {
        code.extend_from_slice(&[0xc6, 0x44, 0x24, offset as u8, val]);
    } else {
        code.push(0x48);
        code.extend_from_slice(&[0xc6, 0x84, 0x24]);
        code.extend_from_slice(&(offset as u32).to_le_bytes());
        code.push(val);
    }
}

/// 生成 tape 初始化代码。
///
/// 使用 mmap 系统调用分配初始缓冲区：
/// ```c
/// void *ptr = mmap(NULL, INITIAL_TAPE_SIZE,
///                  PROT_READ | PROT_WRITE,
///                  MAP_PRIVATE | MAP_ANONYMOUS,
///                  -1, 0);
/// ```
///
/// 分配成功后，初始化三个核心寄存器：
/// - R12 = ptr           （tape 基址）
/// - R13 = ptr + size/2  （数据指针，初始位于 tape 中间）
/// - R14 = ptr + size    （tape 末尾）
///
/// 将指针初始化在中间而非开头，是为了支持 BF 程序向左移动指针（'<'），
/// 而无需立即触发扩容。
fn emit_init_tape(out: &mut Vec<AsmInst>, exit_one_label: AsmLabel) {
    // ---- mmap 系统调用参数 ----
    out.push(AsmInst::MovRegImm64(Reg64::Rax, 9)); // sys_mmap = 9
    out.push(AsmInst::MovRegImm64(Reg64::Rdi, 0)); // addr = NULL（让内核选择地址）
    out.push(AsmInst::MovRegImm64(
        // length = INITIAL_TAPE_SIZE
        Reg64::Rsi,
        INITIAL_TAPE_SIZE as i64,
    ));
    out.push(AsmInst::MovRegImm64(Reg64::Rdx, 0x3)); // prot = PROT_READ(1) | PROT_WRITE(2)
    out.push(AsmInst::MovRegImm64(
        // flags = MAP_PRIVATE(0x02) | MAP_ANONYMOUS(0x20)
        Reg64::R10,
        0x22,
    ));
    out.push(AsmInst::MovRegImm64(Reg64::R8, -1)); // fd = -1（匿名映射）
    out.push(AsmInst::MovRegImm64(Reg64::R9, 0)); // offset = 0
    out.push(AsmInst::Syscall);

    // ---- 检查 mmap 返回值 ----
    // 返回值 < 0 表示错误（如 ENOMEM）
    out.push(AsmInst::CmpRegImm32(Reg64::Rax, 0));
    out.push(AsmInst::Jl(exit_one_label));

    // ---- 初始化寄存器 ----
    // R12 = tape_base = mmap 返回的地址
    out.push(AsmInst::MovRegReg(Reg64::R12, Reg64::Rax));

    // R13 = data_ptr = base + size/2（初始位于中间）
    out.push(AsmInst::MovRegReg(Reg64::R13, Reg64::Rax));
    out.push(AsmInst::AddRegImm32(
        Reg64::R13,
        (INITIAL_TAPE_SIZE / 2) as i32,
    ));

    // R14 = tape_end = base + size
    out.push(AsmInst::MovRegReg(Reg64::R14, Reg64::Rax));
    out.push(AsmInst::AddRegImm32(Reg64::R14, INITIAL_TAPE_SIZE as i32));
}

/// 生成 tape 扩容函数 `ensure_tape_contains_r15`。
///
/// 前置条件：R15 包含目标地址（可能在当前 tape 范围之外）。
///
/// ## 算法
///
/// 1. 计算 old_len = R14 - R12
/// 2. 计算 desired_offset = R15 - R12（可以是负数，表示向左越界）
/// 3. 反复将 new_len 翻倍，直到：
///    - copy_start = (new_len - old_len) / 2（旧数据在新 tape 中的起始偏移）
///    - copy_start + desired_offset >= 0 且 < new_len
/// 4. mmap 分配 new_len 大小的新缓冲区
/// 5. 将旧数据复制到新缓冲区的 copy_start 位置（居中）
/// 6. munmap 释放旧缓冲区
/// 7. 更新 R12 = new_base, R13 = new_base + copy_start + desired_offset, R14 = new_base + new_len
///
/// 后置条件：R12/R13/R14 指向新 tape，R13 位于有效范围内。
fn emit_ensure_tape_contains_r15(
    out: &mut Vec<AsmInst>,
    ensure_tape_label: AsmLabel,
    exit_one_label: AsmLabel,
) {
    let grow_loop = AsmLabel(INTERNAL_LABEL_GROW_LOOP_RAW);

    // ---- 函数入口 ----
    out.push(AsmInst::Label(ensure_tape_label));

    // R10 = old_len = tape_end - tape_base
    out.push(AsmInst::MovRegReg(Reg64::R10, Reg64::R14));
    out.push(AsmInst::SubRegReg(Reg64::R10, Reg64::R12));

    // R9 = desired_offset = target_ptr - tape_base
    // 注意：这可能是负数（如果 R15 < R12）
    out.push(AsmInst::MovRegReg(Reg64::R9, Reg64::R15));
    out.push(AsmInst::SubRegReg(Reg64::R9, Reg64::R12));

    // R11 = new_len（候选值，从 old_len 开始，每次翻倍）
    out.push(AsmInst::MovRegReg(Reg64::R11, Reg64::R10));

    // ---- 翻倍循环 ----
    out.push(AsmInst::Label(grow_loop));

    // new_len *= 2（自加实现翻倍）
    out.push(AsmInst::AddRegReg(Reg64::R11, Reg64::R11));

    // R8 = copy_start = (new_len - old_len) / 2
    // 旧数据将被放置在新缓冲区的这个偏移处（居中对齐）
    out.push(AsmInst::MovRegReg(Reg64::R8, Reg64::R11));
    out.push(AsmInst::SubRegReg(Reg64::R8, Reg64::R10));
    out.push(AsmInst::ShrRegImm8(Reg64::R8, 1));

    // 检查：copy_start + desired_offset 是否在 [0, new_len) 范围内
    // RAX = copy_start + desired_offset
    out.push(AsmInst::MovRegReg(Reg64::Rax, Reg64::R8));
    out.push(AsmInst::AddRegReg(Reg64::Rax, Reg64::R9));

    // 如果 rax < 0（有符号），说明 new_len 还不够大
    out.push(AsmInst::CmpRegImm32(Reg64::Rax, 0));
    out.push(AsmInst::Jl(grow_loop));

    // 如果 rax >= new_len（有符号），说明 new_len 还不够大
    out.push(AsmInst::CmpRegReg(Reg64::Rax, Reg64::R11));
    out.push(AsmInst::Jge(grow_loop));

    // ---- 分配新缓冲区 ----
    // mmap(NULL, new_len, PROT_READ|PROT_WRITE, MAP_PRIVATE|MAP_ANONYMOUS, -1, 0)
    out.push(AsmInst::MovRegImm64(Reg64::Rax, 9)); // sys_mmap
    out.push(AsmInst::MovRegImm64(Reg64::Rdi, 0)); // addr = NULL
    out.push(AsmInst::MovRegReg(Reg64::Rsi, Reg64::R11)); // length = new_len
    out.push(AsmInst::MovRegImm64(Reg64::Rdx, 0x3)); // prot
    out.push(AsmInst::MovRegImm64(Reg64::R10, 0x22)); // flags
    out.push(AsmInst::MovRegImm64(Reg64::R8, -1)); // fd
    out.push(AsmInst::MovRegImm64(Reg64::R9, 0)); // offset
    out.push(AsmInst::Syscall);

    // 检查 mmap 返回值
    out.push(AsmInst::CmpRegImm32(Reg64::Rax, 0));
    out.push(AsmInst::Jl(exit_one_label));

    // ---- 重新计算被 syscall 覆写的值 ----
    //
    // Linux x86_64 syscall 会覆写 rcx（保存旧 RIP）和 r11（保存旧 RFLAGS），
    // 同时 mmap 的参数寄存器 r8/r9/r10 也已被使用。
    // 但 rsi 在 syscall 后仍保留着 new_len。
    //
    // 需要重新计算：
    // - R10 = old_len（从 R14 - R12 重新计算）
    // - R9  = desired_offset（从 R15 - R12 重新计算）
    // - R11 = new_len（从 rsi 恢复，因为 rsi 在 syscall 后未被覆写）
    // - R8  = copy_start（从 (new_len - old_len) / 2 重新计算）
    out.push(AsmInst::MovRegReg(Reg64::R10, Reg64::R14));
    out.push(AsmInst::SubRegReg(Reg64::R10, Reg64::R12));
    out.push(AsmInst::MovRegReg(Reg64::R9, Reg64::R15));
    out.push(AsmInst::SubRegReg(Reg64::R9, Reg64::R12));
    out.push(AsmInst::MovRegReg(Reg64::R11, Reg64::Rsi)); // rsi 保存了 new_len
    out.push(AsmInst::MovRegReg(Reg64::R8, Reg64::R11));
    out.push(AsmInst::SubRegReg(Reg64::R8, Reg64::R10));
    out.push(AsmInst::ShrRegImm8(Reg64::R8, 1));

    // RDX = new_base（mmap 返回的新缓冲区地址）
    out.push(AsmInst::MovRegReg(Reg64::Rdx, Reg64::Rax));

    // ---- 复制旧数据到新缓冲区 ----
    // rep movsb: 复制 old_len 字节，从 old_base 到 new_base + copy_start
    //
    // rep movsb 的参数：
    // - RDI = 目标地址 = new_base + copy_start
    // - RSI = 源地址 = old_base (R12)
    // - RCX = 字节数 = old_len (R10)
    out.push(AsmInst::MovRegReg(Reg64::Rdi, Reg64::Rdx)); // rdi = new_base
    out.push(AsmInst::AddRegReg(Reg64::Rdi, Reg64::R8)); // rdi += copy_start
    out.push(AsmInst::MovRegReg(Reg64::Rsi, Reg64::R12)); // rsi = old_base
    out.push(AsmInst::MovRegReg(Reg64::Rcx, Reg64::R10)); // rcx = old_len
    out.push(AsmInst::Cld); // 清除方向标志（确保向前复制）
    out.push(AsmInst::RepMovsb); // 执行复制

    // ---- 在 munmap 之前先准备好新 tape 的结束地址 ----
    // syscall 会覆写 r11，因此不能在 munmap 之后再依赖 new_len。
    out.push(AsmInst::MovRegReg(Reg64::R14, Reg64::Rdx));
    out.push(AsmInst::AddRegReg(Reg64::R14, Reg64::R11)); // r14 = new_base + new_len

    // ---- 释放旧缓冲区 ----
    // munmap(old_base, old_len)
    out.push(AsmInst::MovRegImm64(Reg64::Rax, 11)); // sys_munmap = 11
    out.push(AsmInst::MovRegReg(Reg64::Rdi, Reg64::R12)); // addr = old_base
    out.push(AsmInst::MovRegReg(Reg64::Rsi, Reg64::R10)); // length = old_len
    out.push(AsmInst::Syscall);
    out.push(AsmInst::CmpRegImm32(Reg64::Rax, 0));
    out.push(AsmInst::Jl(exit_one_label));

    // ---- 更新核心寄存器 ----
    // R12 = new_base
    out.push(AsmInst::MovRegReg(Reg64::R12, Reg64::Rdx));

    // R13 = new_base + copy_start + desired_offset
    // 这是目标地址在新 tape 中的位置
    out.push(AsmInst::MovRegReg(Reg64::R13, Reg64::Rdx));
    out.push(AsmInst::AddRegReg(Reg64::R13, Reg64::R8)); // + copy_start
    out.push(AsmInst::AddRegReg(Reg64::R13, Reg64::R9)); // + desired_offset

    // 返回到调用者（PtrAdd 的 slow_path 中的 CALL 之后）
    out.push(AsmInst::Ret);
}

/// 生成 OOM 退出代码：exit(1)。
///
/// 当 mmap 分配失败时跳转到这里，以非零退出码终止程序。
fn emit_exit_one(out: &mut Vec<AsmInst>, label: AsmLabel) {
    out.push(AsmInst::Label(label));
    out.push(AsmInst::MovRegImm64(Reg64::Rax, 60)); // sys_exit = 60
    out.push(AsmInst::MovRegImm64(Reg64::Rdi, 1)); // 退出码 = 1
    out.push(AsmInst::Syscall);
}

/// 分配一个新的内部标签 ID。
///
/// 从 `next_raw` 的当前值取出一个标签，然后递减。
/// 临时内部标签必须始终落在“保留内部标签区间”以下，
/// 否则会与具有固定语义的内部标签（如 __grow_loop）发生冲突。
fn fresh_internal_label(next_raw: &mut u32) -> AsmLabel {
    debug_assert!(
        *next_raw < INTERNAL_LABEL_RESERVED_MIN_RAW,
        "temporary internal label collided with reserved internal labels: raw=0x{next:08x}",
        next = *next_raw,
    );

    let label = AsmLabel(*next_raw);
    *next_raw -= 1;
    label
}

/// 将 LIR 的 `LabelId` 映射为 `AsmLabel`。
///
/// 直接使用 LabelId 的内部值作为 AsmLabel 的 ID。
/// 由于用户标签从 0 递增，内部标签从 u32::MAX 递减，
/// 两者不会冲突。
fn map_label(id: LabelId) -> AsmLabel {
    AsmLabel(id.0)
}

fn emit_add_reg_isize(out: &mut Vec<AsmInst>, reg: Reg64, value: isize) {
    let mut remaining = i64::try_from(value).expect("pointer delta did not fit in i64");

    while remaining != 0 {
        let chunk = if remaining > i64::from(i32::MAX) {
            i32::MAX
        } else if remaining < i64::from(i32::MIN) {
            i32::MIN
        } else {
            remaining as i32
        };

        out.push(AsmInst::AddRegImm32(reg, chunk));
        remaining -= i64::from(chunk);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::lir::{LirInst, LirProgram};

    #[test]
    fn get_byte_sets_eof_cell_to_255() {
        let asm = compile_lir_to_asm(&LirProgram {
            insts: vec![LirInst::GetByte],
        });

        let mut matched = false;
        for window in asm.insts.windows(5) {
            matched = matches!(
                window,
                [
                    AsmInst::CmpRegImm32(Reg64::Rax, 0),
                    AsmInst::Jl(_),
                    AsmInst::Jnz(_),
                    AsmInst::MovMem8Imm8(Reg64::R13, 255),
                    AsmInst::Label(_)
                ]
            );

            if matched {
                break;
            }
        }

        assert!(matched, "GetByte should map EOF to 255 in generated asm");
    }

    #[test]
    fn ptr_add_is_emitted_without_i32_truncation() {
        let large_delta = isize::try_from(i64::from(i32::MAX) + 5).unwrap();
        let asm = compile_lir_to_asm(&LirProgram {
            insts: vec![LirInst::PtrAdd(large_delta)],
        });

        let chunks: Vec<i32> = asm
            .insts
            .iter()
            .filter_map(|inst| match inst {
                AsmInst::AddRegImm32(Reg64::R15, imm) => Some(*imm),
                _ => None,
            })
            .collect();

        assert_eq!(chunks, vec![i32::MAX, 5]);
    }

    #[test]
    fn precomputed_stdout_asm_encodes_large_stack_offsets() {
        let data: Vec<u8> = (0..130).map(|i| (i % 256) as u8).collect();
        let asm = compile_precomputed_stdout_asm(&data);
        let encoded = crate::backend::x86_64::encode::encode_program(&asm);
        assert!(
            encoded.text.len() > 400,
            "130 output bytes should use disp8 + disp32 mov-to-[rsp+i] encodings"
        );
    }

    #[test]
    fn ensure_tape_does_not_use_r11_after_munmap_syscall() {
        let asm = compile_lir_to_asm(&LirProgram {
            insts: vec![LirInst::PtrAdd(4096)],
        });

        let munmap_idx = asm
            .insts
            .iter()
            .position(|inst| matches!(inst, AsmInst::MovRegImm64(Reg64::Rax, 11)))
            .expect("expected munmap syscall sequence");

        let tail = &asm.insts[munmap_idx..];
        let mut saw_syscall = false;

        for inst in tail {
            if matches!(inst, AsmInst::Syscall) {
                saw_syscall = true;
                continue;
            }

            if saw_syscall {
                assert!(
                    !matches!(inst, AsmInst::AddRegReg(Reg64::R14, Reg64::R11)),
                    "r11 is clobbered by syscall and must not be used after munmap"
                );
            }
        }
    }
}
