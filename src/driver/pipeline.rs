//! Shared frontend pipeline from source text to optimized HIR.

use anyhow::Result;
use tracing::debug;

use crate::driver::config::{DriverConfig, OptLevel};
use crate::frontend::lexer::lex;
use crate::frontend::parser::parse;
use crate::ir::hir::HirProgram;
use crate::ir::lower::lower_to_hir;
use crate::ir::optimize::{optimize_o0, optimize_o1, try_optimize_o2};

pub(crate) struct FrontendArtifacts {
    pub(crate) token_count: usize,
    pub(crate) ast_nodes: usize,
    pub(crate) hir: HirProgram,
}

pub(crate) fn build_frontend(config: &DriverConfig) -> Result<FrontendArtifacts> {
    let tokens = lex(&config.source);
    debug!(token_count = tokens.len(), "lexed source");

    let ast = parse(&tokens)?;
    debug!(ast_nodes = ast.len(), "parsed ast");

    let hir = match config.opt_level {
        OptLevel::O0 => optimize_o0(lower_to_hir(&ast)),
        OptLevel::O1 => optimize_o1(lower_to_hir(&ast)),
        OptLevel::O2 | OptLevel::O3 => try_optimize_o2(lower_to_hir(&ast))?,
    };
    debug!(hir_insts = hir.insts.len(), "lowered and optimized hir");

    Ok(FrontendArtifacts {
        token_count: tokens.len(),
        ast_nodes: ast.len(),
        hir,
    })
}
