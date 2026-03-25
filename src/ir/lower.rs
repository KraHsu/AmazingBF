use crate::frontend::ast::AstNode;
use crate::ir::hir::{HirInst, HirProgram};
use crate::ir::lir::{LabelGen, LirInst, LirProgram};

/// from ast to hir
///
/// - MoveRight / MoveLeft -> Move(+1/-1)
/// - Inc / Dec            -> Add(+1/-1)
pub fn lower_to_hir(ast: &[AstNode]) -> HirProgram {
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

/// from hir to lir
///
/// - Move(n)    -> PtrAdd(n)
/// - Add(n)     -> CellAdd(n)
/// - PutByte    -> PutByte
/// - GetByte    -> GetByte
/// - Loop(body) -> explicit labels and jumps
pub fn lower_to_lir(hir: &HirProgram) -> LirProgram {
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
