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

    match config.mode {
        RunMode::DumpIr => {
            println!("== TOKENS ==\n{:#?}", tokens);
            println!("== AST ==\n{:#?}", ast);
            println!("== HIR ==\n{:#?}", hir);
            println!("== LIR ==\n{:#?}", lir);
        }
        RunMode::Interpret => {
            let io = StdIo::new();
            let host = NullHost::new();
            let mut interp = Interpreter::new(30_000, io, host);

            interp.run(&hir)?;
        }
    }

    Ok(())
}
