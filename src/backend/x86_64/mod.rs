pub mod elf;
pub mod encode;

use crate::backend::codegen::compile_lir_to_asm;
use crate::ir::lir::LirProgram;

pub fn compile_lir_to_elf(lir: &LirProgram) -> Vec<u8> {
    let asm = compile_lir_to_asm(lir);
    let encoded = encode::encode_program(&asm);
    elf::build_elf_executable(&encoded)
}
