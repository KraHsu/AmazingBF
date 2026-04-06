//! Shared frontend pipeline from source text to optimized HIR.

use crate::Result;
use crate::driver::config::{DriverConfig, OptLevel};
use crate::frontend::lexer::lex;
use crate::frontend::parser::parse;
use crate::ir::hir::HirProgram;
use crate::ir::lower::lower_to_hir;
use crate::ir::optimize::{optimize_o0, optimize_o1, try_optimize_o2};
use crate::logging::log_debug;

pub(crate) struct FrontendArtifacts {
    pub(crate) token_count: usize,
    pub(crate) ast_nodes: usize,
    pub(crate) hir: HirProgram,
}

pub(crate) fn build_frontend(config: &DriverConfig) -> Result<FrontendArtifacts> {
    let tokens = lex(&config.source);
    log_debug(format!("lexed source (token_count={})", tokens.len()));

    let ast = parse(&tokens)?;
    log_debug(format!("parsed ast (ast_nodes={})", ast.len()));

    let hir = match config.opt_level {
        OptLevel::O0 => optimize_o0(lower_to_hir(&ast)),
        OptLevel::O1 => optimize_o1(lower_to_hir(&ast)),
        OptLevel::O2 | OptLevel::O3 => try_optimize_o2(lower_to_hir(&ast))?,
    };
    log_debug(format!(
        "lowered and optimized hir (hir_insts={})",
        hir.insts.len()
    ));

    Ok(FrontendArtifacts {
        token_count: tokens.len(),
        ast_nodes: ast.len(),
        hir,
    })
}
