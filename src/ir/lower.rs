//! Lowering passes: AST → HIR and HIR → LIR.
//!
//! `lower_to_hir` flattens the parsed AST into the HIR instruction list used
//! by optimization and interpretation. `lower_to_lir` then linearises HIR
//! loops into labelled jumps so the backend can emit native conditional
//! branches directly.

use crate::frontend::ast::AstNode;
use crate::ir::hir::{HirInst, HirProgram};
use crate::ir::lir::{LabelGen, LirInst, LirProgram};

/// Lower the parsed AST into the high-level IR used by optimization and interpretation.
pub(crate) fn lower_to_hir(ast: &[AstNode]) -> HirProgram {
    HirProgram {
        insts: lower_to_hir_block(ast),
    }
}

fn lower_to_hir_block(ast: &[AstNode]) -> Vec<HirInst> {
    let mut out = Vec::new();

    for node in ast {
        match node {
            AstNode::MoveRight => out.push(HirInst::Move(1)),
            AstNode::MoveLeft => out.push(HirInst::Move(-1)),
            AstNode::Inc => out.push(HirInst::Add(1)),
            AstNode::Dec => out.push(HirInst::Add(-1)),
            AstNode::Output => out.push(HirInst::PutByte),
            AstNode::Input => out.push(HirInst::GetByte),
            AstNode::Loop(body) => {
                out.push(HirInst::Loop(lower_to_hir_block(body)));
            }
        }
    }

    out
}

/// Lower HIR into the backend-facing low-level IR with explicit loop labels and jumps.
pub(crate) fn lower_to_lir(hir: &HirProgram) -> LirProgram {
    let mut labels = LabelGen::new();

    LirProgram {
        insts: lower_to_lir_block(&hir.insts, &mut labels),
    }
}

fn lower_to_lir_block(hir: &[HirInst], labels: &mut LabelGen) -> Vec<LirInst> {
    let mut out = Vec::new();

    for inst in hir {
        match inst {
            HirInst::Move(0) => {}
            HirInst::Move(n) => out.push(LirInst::PtrAdd(*n)),

            HirInst::Add(0) => {}
            HirInst::Add(n) => out.push(LirInst::CellAdd(*n)),

            HirInst::PutByte => out.push(LirInst::PutByte),
            HirInst::GetByte => out.push(LirInst::GetByte),

            HirInst::Zero => out.push(LirInst::CellSet(0)),

            HirInst::LinearMul(factors) => out.push(LirInst::LinearMul(factors.clone())),

            HirInst::Scan(dir) => out.push(LirInst::Scan(*dir)),

            HirInst::Loop(body) => {
                let begin = labels.fresh();
                let end = labels.fresh();

                out.push(LirInst::Label(begin));
                out.push(LirInst::JumpIfZero(end));
                out.extend(lower_to_lir_block(body, labels));
                out.push(LirInst::JumpIfNonZero(begin));
                out.push(LirInst::Label(end));
            }
        }
    }

    out
}
