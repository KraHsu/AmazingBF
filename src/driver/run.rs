use crate::driver::config::DriverConfig;
use crate::frontend::lexer::lex;
use crate::frontend::parser::parse;
use crate::ir::lower::lower;
use crate::ir::optimize::optimize;

/// from src to HIR
pub fn run(config: DriverConfig) -> Result<(), String> {
    let tokens = lex(&config.source);
    println!("== TOKENS ==\n{:#?}", tokens);

    let ast = parse(&tokens).map_err(|e| format!("parse error: {:?}", e))?;
    println!("== AST ==\n{:#?}", ast);

    let program = lower(&ast);
    println!("== HIR (before optimize) ==\n{:#?}", program);

    let program = optimize(program);
    println!("== HIR (after optimize) ==\n{:#?}", program);

    Ok(())
}
