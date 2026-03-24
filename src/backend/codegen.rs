use crate::backend::asm::{AsmInst, AsmLabel, AsmProgram, Reg64};
use crate::ir::lir::{LabelId, LirInst, LirProgram};

const DEFAULT_TAPE_SIZE: usize = 30_000;
const TAPE_LABEL_RAW: u32 = u32::MAX;

pub fn compile_lir_to_asm(lir: &LirProgram) -> AsmProgram {
    let tape_label = AsmLabel(TAPE_LABEL_RAW);
    let mut out = Vec::new();

    // r13 = &tape
    out.push(AsmInst::LeaRipLabel(Reg64::R13, tape_label));

    for inst in &lir.insts {
        match inst {
            LirInst::PtrAdd(0) => {}
            LirInst::PtrAdd(n) => {
                out.push(AsmInst::AddRegImm32(Reg64::R13, *n as i32));
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

    // tape label
    out.push(AsmInst::Label(tape_label));

    AsmProgram {
        insts: out,
        tape_label,
        tape_size: DEFAULT_TAPE_SIZE,
    }
}

fn map_label(id: LabelId) -> AsmLabel {
    AsmLabel(id.0)
}
