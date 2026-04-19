use super::BfscError;
use super::ast::*;
use super::layout::{MemMap, MemMapBuilder};
use std::collections::HashMap;

pub(crate) fn check(stmts: &[Stmt]) -> Result<(Vec<Stmt>, MemMap), BfscError> {
    let mut builder = MemMapBuilder::new();
    let mut sym: HashMap<String, TypeAnn> = HashMap::new();

    // First pass: collect all Let declarations into the memory map
    for stmt in stmts {
        collect_decls(stmt, &mut sym, &mut builder)?;
    }

    let map = builder.finalize();

    // Second pass: type-check all statements
    for stmt in stmts {
        check_stmt(stmt, &sym)?;
    }

    Ok((stmts.to_vec(), map))
}

fn collect_decls(
    stmt: &Stmt,
    sym: &mut HashMap<String, TypeAnn>,
    builder: &mut MemMapBuilder,
) -> Result<(), BfscError> {
    match stmt {
        Stmt::Let { name, ty, .. } => {
            if sym.contains_key(name.as_str()) {
                return Err(BfscError::Type(format!("duplicate variable '{name}'")));
            }
            match ty {
                TypeAnn::Scalar(st) => builder.alloc_scalar(name.clone(), *st),
                TypeAnn::Array(st, len) => builder.alloc_array(name.clone(), *st, *len as usize),
            }
            sym.insert(name.clone(), ty.clone());
        }
        Stmt::While { body, .. } | Stmt::If { then_: body, .. } => {
            for s in body {
                collect_decls(s, sym, builder)?;
            }
            if let Stmt::If {
                else_: Some(eb), ..
            } = stmt
            {
                for s in eb {
                    collect_decls(s, sym, builder)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn check_stmt(stmt: &Stmt, sym: &HashMap<String, TypeAnn>) -> Result<(), BfscError> {
    match stmt {
        Stmt::Let { name, init, .. } => {
            if let Some(expr) = init {
                check_expr(expr, sym)?;
                let _ = name; // already declared
            }
        }
        Stmt::Assign { lval, expr } => {
            check_lval(lval, sym)?;
            check_expr(expr, sym)?;
        }
        Stmt::While { cond, body } => {
            check_expr(cond, sym)?;
            for s in body {
                check_stmt(s, sym)?;
            }
        }
        Stmt::If { cond, then_, else_ } => {
            check_expr(cond, sym)?;
            for s in then_ {
                check_stmt(s, sym)?;
            }
            if let Some(eb) = else_ {
                for s in eb {
                    check_stmt(s, sym)?;
                }
            }
        }
        Stmt::Scan(lval) => check_lval(lval, sym)?,
        Stmt::Print(expr) | Stmt::Putchar(expr) => check_expr(expr, sym)?,
        Stmt::Setpixel { x, y, color } => {
            check_expr(x, sym)?;
            check_expr(y, sym)?;
            check_expr(color, sym)?;
        }
        Stmt::Getchar(lval) => check_lval(lval, sym)?,
    }
    Ok(())
}

fn check_lval(lval: &LValue, sym: &HashMap<String, TypeAnn>) -> Result<(), BfscError> {
    match lval {
        LValue::Var(name) => {
            if !sym.contains_key(name.as_str()) {
                return Err(BfscError::Type(format!("undeclared variable '{name}'")));
            }
        }
        LValue::Index(name, idx) => {
            match sym.get(name.as_str()) {
                None => return Err(BfscError::Type(format!("undeclared variable '{name}'"))),
                Some(TypeAnn::Scalar(_)) => {
                    return Err(BfscError::Type(format!("'{name}' is not an array")));
                }
                Some(TypeAnn::Array(_, _)) => {}
            }
            check_expr(idx, sym)?;
        }
    }
    Ok(())
}

fn check_expr(expr: &Expr, sym: &HashMap<String, TypeAnn>) -> Result<(), BfscError> {
    match expr {
        Expr::Int(_) => {}
        Expr::Var(name) => {
            if !sym.contains_key(name.as_str()) {
                return Err(BfscError::Type(format!("undeclared variable '{name}'")));
            }
        }
        Expr::Index(name, idx) => {
            match sym.get(name.as_str()) {
                None => return Err(BfscError::Type(format!("undeclared variable '{name}'"))),
                Some(TypeAnn::Scalar(_)) => {
                    return Err(BfscError::Type(format!("'{name}' is not an array")));
                }
                Some(TypeAnn::Array(_, _)) => {}
            }
            check_expr(idx, sym)?;
        }
        Expr::BinOp(_, l, r) => {
            check_expr(l, sym)?;
            check_expr(r, sym)?;
        }
        Expr::UnOp(_, e) => check_expr(e, sym)?,
    }
    Ok(())
}
