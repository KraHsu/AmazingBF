use std::fs;
use std::os::unix::fs::PermissionsExt;

use crate::backend::codegen::compile_lir_to_asm;
use crate::backend::x86_64::compile_lir_to_elf;
use crate::driver::config::{DriverConfig, RunMode};
use crate::frontend::lexer::lex;
use crate::frontend::parser::parse;
use crate::interp::engine::Interpreter;
use crate::ir::lower::{lower_to_hir, lower_to_lir};
use crate::ir::optimize::optimize;
use crate::runtime::host::NullHost;
use crate::runtime::io::StdIo;
use anyhow::Result;

/// from src to HIR
pub fn run(config: DriverConfig) -> Result<()> {
    let tokens = lex(&config.source);
    let ast = parse(&tokens)?;
    let hir = optimize(lower_to_hir(&ast));
    let lir = lower_to_lir(&hir);
    let asm = compile_lir_to_asm(&lir);

    match config.mode {
        RunMode::Dump => {
            println!("== TOKENS ==\n{:#?}", tokens);
            println!("== AST ==\n{:#?}", ast);
            println!("== HIR ==\n{:#?}", hir);
            println!("== LIR ==\n{:#?}", lir);
            println!("== ASM ==\n{:#?}", asm);
        }
        RunMode::Interpret => {
            let io = StdIo::new();
            let host = NullHost::new();
            let mut interp = Interpreter::new(30_000, io, host);

            interp.run(&hir)?;
        }
        RunMode::ToElf => {
            let elf = compile_lir_to_elf(&lir);
            fs::write("a.out", &elf)?;
            let mut perms = fs::metadata("a.out")?.permissions();
            perms.set_mode(0o755);
            fs::set_permissions("a.out", perms)?;
        }
    }

    Ok(())
}
