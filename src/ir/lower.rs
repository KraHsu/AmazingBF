use crate::frontend::ast::AstNode;
use crate::ir::hir::{HirInst, Program};

/// from ast to hir
///
/// - MoveRight / MoveLeft -> Move(+1/-1)
/// - Inc / Dec            -> Add(+1/-1)
pub fn lower(ast: &[AstNode]) -> Program {
    Program {
        insts: lower_block(ast),
    }
}

fn lower_block(ast: &[AstNode]) -> Vec<HirInst> {
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
                out.push(HirInst::Loop(lower_block(body)));
            }
        }
    }

    out
}
