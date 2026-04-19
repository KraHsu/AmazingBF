//! Shared frontend pipeline from source text to optimized HIR.

use crate::Result;
use crate::driver::config::{DriverConfig, OptLevel};
use crate::frontend::lexer::lex;
use crate::frontend::parser::parse;
use crate::ir::hir::HirProgram;
use crate::ir::lower::lower_to_hir;
use crate::ir::optimize::{optimize_o0, optimize_o1, try_optimize_o2};
use crate::logging::log_debug;

/// Bundle produced by the shared frontend pipeline, passed from driver to interpreter or backend.
pub(crate) struct FrontendArtifacts {
    /// Number of tokens produced by the lexer (for diagnostics / logging).
    pub(crate) token_count: usize,
    /// Number of AST top-level nodes produced by the parser.
    pub(crate) ast_nodes: usize,
    /// Optimized HIR program ready for interpretation or LIR lowering.
    pub(crate) hir: HirProgram,
}

/// Run the shared `lex → parse → lower → optimize` pipeline and return the
/// artifacts consumed by both `RunMode::Interpret` and the native backend.
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
