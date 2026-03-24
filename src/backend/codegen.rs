use crate::backend::asm::{AsmInst, AsmLabel, AsmProgram, Reg64};
use crate::ir::lir::{LabelId, LirInst, LirProgram};

const INITIAL_TAPE_SIZE: usize = 4096;
const INTERNAL_LABEL_ENSURE_TAPE_RAW: u32 = u32::MAX;
const INTERNAL_LABEL_OOM_EXIT_RAW: u32 = u32::MAX - 1;
const INTERNAL_LABEL_BASE_RAW: u32 = u32::MAX - 2;
const INTERNAL_LABEL_GROW_LOOP_RAW: u32 = u32::MAX - 10_000;

pub fn compile_lir_to_asm(lir: &LirProgram) -> AsmProgram {
    let ensure_tape_label = AsmLabel(INTERNAL_LABEL_ENSURE_TAPE_RAW);
    let oom_exit_label = AsmLabel(INTERNAL_LABEL_OOM_EXIT_RAW);
    let mut next_internal_label = INTERNAL_LABEL_BASE_RAW;
    let mut out = Vec::new();

    emit_init_tape(&mut out, oom_exit_label);

    for inst in &lir.insts {
        match inst {
            LirInst::PtrAdd(0) => {}
            LirInst::PtrAdd(n) => {
                let slow_path = fresh_internal_label(&mut next_internal_label);
                let done = fresh_internal_label(&mut next_internal_label);

                out.push(AsmInst::MovRegReg(Reg64::R15, Reg64::R13));
                out.push(AsmInst::AddRegImm32(Reg64::R15, *n as i32));
                out.push(AsmInst::CmpRegReg(Reg64::R15, Reg64::R12));
                out.push(AsmInst::Jb(slow_path));
                out.push(AsmInst::CmpRegReg(Reg64::R15, Reg64::R14));
                out.push(AsmInst::Jae(slow_path));
                out.push(AsmInst::MovRegReg(Reg64::R13, Reg64::R15));
                out.push(AsmInst::Jmp(done));
                out.push(AsmInst::Label(slow_path));
                out.push(AsmInst::Call(ensure_tape_label));
                out.push(AsmInst::Label(done));
            }

            LirInst::CellAdd(0) => {}
            LirInst::CellAdd(n) => {
                let imm = ((*n % 256) + 256) % 256;
                if imm != 0 {
                    out.push(AsmInst::AddMem8Imm8(Reg64::R13, imm as u8 as i8));
                }
            }

            LirInst::CellSet(v) => {
                out.push(AsmInst::MovMem8Imm8(Reg64::R13, *v));
            }

            LirInst::PutByte => {
                out.push(AsmInst::MovRegImm64(Reg64::Rax, 1));
                out.push(AsmInst::MovRegImm64(Reg64::Rdi, 1));
                out.push(AsmInst::MovRegReg(Reg64::Rsi, Reg64::R13));
                out.push(AsmInst::MovRegImm64(Reg64::Rdx, 1));
                out.push(AsmInst::Syscall);
            }

            LirInst::GetByte => {
                out.push(AsmInst::MovRegImm64(Reg64::Rax, 0));
                out.push(AsmInst::MovRegImm64(Reg64::Rdi, 0));
                out.push(AsmInst::MovRegReg(Reg64::Rsi, Reg64::R13));
                out.push(AsmInst::MovRegImm64(Reg64::Rdx, 1));
                out.push(AsmInst::Syscall);
            }

            LirInst::Label(id) => {
                out.push(AsmInst::Label(map_label(*id)));
            }

            LirInst::JumpIfZero(id) => {
                out.push(AsmInst::CmpMem8Imm8(Reg64::R13, 0));
                out.push(AsmInst::Jz(map_label(*id)));
            }

            LirInst::JumpIfNonZero(id) => {
                out.push(AsmInst::CmpMem8Imm8(Reg64::R13, 0));
                out.push(AsmInst::Jnz(map_label(*id)));
            }

            #[allow(unreachable_patterns)]
            _ => {
                panic!("unsupported LIR instruction in backend");
            }
        }
    }

    out.push(AsmInst::MovRegImm64(Reg64::Rax, 60));
    out.push(AsmInst::MovRegImm64(Reg64::Rdi, 0));
    out.push(AsmInst::Syscall);

    emit_ensure_tape_contains_r15(&mut out, ensure_tape_label, oom_exit_label);
    emit_exit_one(&mut out, oom_exit_label);

    AsmProgram { insts: out }
}

fn emit_init_tape(out: &mut Vec<AsmInst>, oom_exit_label: AsmLabel) {
    out.push(AsmInst::MovRegImm64(Reg64::Rax, 9));
    out.push(AsmInst::MovRegImm64(Reg64::Rdi, 0));
    out.push(AsmInst::MovRegImm64(Reg64::Rsi, INITIAL_TAPE_SIZE as i64));
    out.push(AsmInst::MovRegImm64(Reg64::Rdx, 0x3));
    out.push(AsmInst::MovRegImm64(Reg64::R10, 0x22));
    out.push(AsmInst::MovRegImm64(Reg64::R8, -1));
    out.push(AsmInst::MovRegImm64(Reg64::R9, 0));
    out.push(AsmInst::Syscall);
    out.push(AsmInst::CmpRegImm32(Reg64::Rax, 0));
    out.push(AsmInst::Jl(oom_exit_label));

    out.push(AsmInst::MovRegReg(Reg64::R12, Reg64::Rax));
    out.push(AsmInst::MovRegReg(Reg64::R13, Reg64::Rax));
    out.push(AsmInst::AddRegImm32(
        Reg64::R13,
        (INITIAL_TAPE_SIZE / 2) as i32,
    ));
    out.push(AsmInst::MovRegReg(Reg64::R14, Reg64::Rax));
    out.push(AsmInst::AddRegImm32(Reg64::R14, INITIAL_TAPE_SIZE as i32));
}

fn emit_ensure_tape_contains_r15(
    out: &mut Vec<AsmInst>,
    ensure_tape_label: AsmLabel,
    oom_exit_label: AsmLabel,
) {
    let grow_loop = AsmLabel(INTERNAL_LABEL_GROW_LOOP_RAW);

    out.push(AsmInst::Label(ensure_tape_label));

    // r10 = old_len = end - base
    out.push(AsmInst::MovRegReg(Reg64::R10, Reg64::R14));
    out.push(AsmInst::SubRegReg(Reg64::R10, Reg64::R12));

    // r9 = desired_offset = desired_ptr - old_base
    out.push(AsmInst::MovRegReg(Reg64::R9, Reg64::R15));
    out.push(AsmInst::SubRegReg(Reg64::R9, Reg64::R12));

    // r11 = new_len candidate
    out.push(AsmInst::MovRegReg(Reg64::R11, Reg64::R10));

    out.push(AsmInst::Label(grow_loop));
    // double until centered copy range can contain desired offset
    out.push(AsmInst::AddRegReg(Reg64::R11, Reg64::R11));
    out.push(AsmInst::MovRegReg(Reg64::R8, Reg64::R11));
    out.push(AsmInst::SubRegReg(Reg64::R8, Reg64::R10));
    out.push(AsmInst::ShrRegImm8(Reg64::R8, 1));

    // rax = start + desired_offset
    out.push(AsmInst::MovRegReg(Reg64::Rax, Reg64::R8));
    out.push(AsmInst::AddRegReg(Reg64::Rax, Reg64::R9));
    out.push(AsmInst::CmpRegImm32(Reg64::Rax, 0));
    out.push(AsmInst::Jl(grow_loop));
    out.push(AsmInst::CmpRegReg(Reg64::Rax, Reg64::R11));
    out.push(AsmInst::Jge(grow_loop));

    // mmap(NULL, new_len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0)
    out.push(AsmInst::MovRegImm64(Reg64::Rax, 9));
    out.push(AsmInst::MovRegImm64(Reg64::Rdi, 0));
    out.push(AsmInst::MovRegReg(Reg64::Rsi, Reg64::R11));
    out.push(AsmInst::MovRegImm64(Reg64::Rdx, 0x3));
    out.push(AsmInst::MovRegImm64(Reg64::R10, 0x22));
    out.push(AsmInst::MovRegImm64(Reg64::R8, -1));
    out.push(AsmInst::MovRegImm64(Reg64::R9, 0));
    out.push(AsmInst::Syscall);
    out.push(AsmInst::CmpRegImm32(Reg64::Rax, 0));
    out.push(AsmInst::Jl(oom_exit_label));

    // Restore old_len / desired_offset / start after syscall clobbers.
    // Linux x86_64 syscall clobbers rcx and r11, while rsi still keeps new_len.
    out.push(AsmInst::MovRegReg(Reg64::R10, Reg64::R14));
    out.push(AsmInst::SubRegReg(Reg64::R10, Reg64::R12));
    out.push(AsmInst::MovRegReg(Reg64::R9, Reg64::R15));
    out.push(AsmInst::SubRegReg(Reg64::R9, Reg64::R12));
    out.push(AsmInst::MovRegReg(Reg64::R11, Reg64::Rsi));
    out.push(AsmInst::MovRegReg(Reg64::R8, Reg64::R11));
    out.push(AsmInst::SubRegReg(Reg64::R8, Reg64::R10));
    out.push(AsmInst::ShrRegImm8(Reg64::R8, 1));

    // rdx = new_base
    out.push(AsmInst::MovRegReg(Reg64::Rdx, Reg64::Rax));

    // rep movsb(new_base + start, old_base, old_len)
    out.push(AsmInst::MovRegReg(Reg64::Rdi, Reg64::Rdx));
    out.push(AsmInst::AddRegReg(Reg64::Rdi, Reg64::R8));
    out.push(AsmInst::MovRegReg(Reg64::Rsi, Reg64::R12));
    out.push(AsmInst::MovRegReg(Reg64::Rcx, Reg64::R10));
    out.push(AsmInst::Cld);
    out.push(AsmInst::RepMovsb);

    // munmap(old_base, old_len)
    out.push(AsmInst::MovRegImm64(Reg64::Rax, 11));
    out.push(AsmInst::MovRegReg(Reg64::Rdi, Reg64::R12));
    out.push(AsmInst::MovRegReg(Reg64::Rsi, Reg64::R10));
    out.push(AsmInst::Syscall);

    // base = new_base
    out.push(AsmInst::MovRegReg(Reg64::R12, Reg64::Rdx));
    // current = new_base + start + desired_offset
    out.push(AsmInst::MovRegReg(Reg64::R13, Reg64::Rdx));
    out.push(AsmInst::AddRegReg(Reg64::R13, Reg64::R8));
    out.push(AsmInst::AddRegReg(Reg64::R13, Reg64::R9));
    // end = new_base + new_len
    out.push(AsmInst::MovRegReg(Reg64::R14, Reg64::Rdx));
    out.push(AsmInst::AddRegReg(Reg64::R14, Reg64::R11));
    out.push(AsmInst::Ret);
}

fn emit_exit_one(out: &mut Vec<AsmInst>, label: AsmLabel) {
    out.push(AsmInst::Label(label));
    out.push(AsmInst::MovRegImm64(Reg64::Rax, 60));
    out.push(AsmInst::MovRegImm64(Reg64::Rdi, 1));
    out.push(AsmInst::Syscall);
}

fn fresh_internal_label(next_raw: &mut u32) -> AsmLabel {
    let label = AsmLabel(*next_raw);
    *next_raw -= 1;
    label
}

fn map_label(id: LabelId) -> AsmLabel {
    AsmLabel(id.0)
}
