use crate::driver::config::{DriverConfig, RunMode};
use crate::frontend::lexer::lex;
use crate::frontend::parser::parse;
use crate::interp::engine::Interpreter;
use crate::ir::lower::lower;
use crate::ir::optimize::optimize;
use crate::runtime::host::NullHost;
use crate::runtime::io::StdIo;

/// from src to HIR
pub fn run(config: DriverConfig) -> Result<(), String> {
    let tokens = lex(&config.source);
    let ast = parse(&tokens).map_err(|e| format!("parse error: {:?}", e))?;
    let program = optimize(lower(&ast));

    match config.mode {
        RunMode::DumpIr => {
            println!("== TOKENS ==\n{:#?}", tokens);
            println!("== AST ==\n{:#?}", ast);
            println!("== HIR ==\n{:#?}", program);
        }
        RunMode::Interpret => {
            let io = StdIo::new();
            let host = NullHost::new();
            let mut interp = Interpreter::new(30_000, io, host);

            interp
                .run(&program)
                .map_err(|e| format!("runtime error: {:?}", e))?;
        }
    }

    Ok(())
}
