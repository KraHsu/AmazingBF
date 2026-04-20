//! BFS type checker and memory-map builder.
//!
//! Three passes over the parsed [`Program`]:
//!   1. Collect top-level `let` bindings into `MemMapBuilder`, and collect all
//!      function signatures into `FnTable`. Function names share the global
//!      namespace with variables (same-name is an error).
//!   2. For each function definition, type-check its body against a local
//!      symbol table seeded with the global variables plus the function's
//!      own parameters. `return` is only permitted as the last statement of
//!      the body, and its type (or absence) must match the declared return
//!      type. Calls are checked for arity and argument-type agreement; array
//!      arguments must be plain array identifiers with matching element type
//!      and length.
//!   3. Type-check the top-level statements, which may reference any
//!      top-level binding or any function.
//!
//! Finally, a DFS over the static call graph rejects (mutual) recursion
//! since the codegen's monomorphizing inliner cannot represent it.

use super::BfscError;
use super::ast::*;
use super::layout::{MemMap, MemMapBuilder};
use std::collections::{HashMap, HashSet};

/// Frozen signature of a BFS function, used by typeck and codegen lookups.
#[derive(Debug, Clone)]
pub(crate) struct FnSig {
    pub(crate) params: Vec<Param>,
    pub(crate) ret_ty: Option<ScalarType>,
}

/// Table of function signatures keyed by function name.
pub(crate) type FnTable = HashMap<String, FnSig>;

/// Type-check `program` and compute a frozen global memory layout. Returns
/// the validated program, the top-level [`MemMap`] consumed by codegen, and
/// the function signature table.
pub(crate) fn check(program: &Program) -> Result<(Program, MemMap, FnTable), BfscError> {
    let mut builder = MemMapBuilder::new();
    let mut sym: HashMap<String, TypeAnn> = HashMap::new();

    // Pass A.1: collect global let bindings.
    for stmt in &program.top {
        collect_decls(stmt, &mut sym, &mut builder)?;
    }

    // Pass A.2: collect function signatures.
    let mut fns: FnTable = HashMap::new();
    for f in &program.fns {
        if fns.contains_key(&f.name) {
            return Err(BfscError::Type(format!("duplicate function '{}'", f.name)));
        }
        if sym.contains_key(&f.name) {
            return Err(BfscError::Type(format!(
                "function '{}' shadows a top-level variable",
                f.name
            )));
        }
        // Check parameters are unique within the signature.
        let mut seen: HashSet<&str> = HashSet::new();
        for p in &f.params {
            if !seen.insert(p.name.as_str()) {
                return Err(BfscError::Type(format!(
                    "duplicate parameter '{}' in function '{}'",
                    p.name, f.name
                )));
            }
        }
        fns.insert(
            f.name.clone(),
            FnSig {
                params: f.params.clone(),
                ret_ty: f.ret_ty,
            },
        );
    }

    // Pass B: check each function body with its own local symbol table. The
    // function's parameters and `let` bindings shadow any same-named top-level
    // binding (codegen's per-call scope stack routes lookups to the inner
    // copy). Duplicates *within* a function — same-named parameters, or a
    // local that collides with a parameter or another local — remain errors.
    for f in &program.fns {
        let mut local = sym.clone();
        let mut in_fn: HashSet<String> = HashSet::new();
        for p in &f.params {
            if !in_fn.insert(p.name.clone()) {
                return Err(BfscError::Type(format!(
                    "duplicate parameter '{}' in function '{}'",
                    p.name, f.name
                )));
            }
            local.insert(p.name.clone(), p.ty.clone());
        }
        check_fn_body(&f.name, &f.body, f.ret_ty, &mut local, &mut in_fn, &fns)?;
    }

    // Pass C: check top-level statements.
    for stmt in &program.top {
        check_stmt(stmt, &sym, &fns, None)?;
    }

    // Reject recursive call graphs (the inliner cannot lower them).
    detect_recursion(&program.fns)?;

    Ok((program.clone(), builder.finalize(), fns))
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

fn check_fn_body(
    fn_name: &str,
    body: &[Stmt],
    ret_ty: Option<ScalarType>,
    sym: &mut HashMap<String, TypeAnn>,
    in_fn: &mut HashSet<String>,
    fns: &FnTable,
) -> Result<(), BfscError> {
    // Enforce: `return` is only allowed as the last statement. Scan all but
    // the last entry for stray returns, then validate the trailing position.
    for (i, s) in body.iter().enumerate() {
        if matches!(s, Stmt::Return(_)) && i + 1 != body.len() {
            return Err(BfscError::Type(format!(
                "in function '{fn_name}': `return` must be the last statement of the body"
            )));
        }
        // Reject returns in nested blocks wholesale — same rule, no early exits.
        ensure_no_nested_return(s, fn_name)?;
    }

    // Register any body-local let bindings into the scope before check_stmt
    // walks them, so later statements can see them. Shadowing of top-level
    // bindings is permitted; duplicates inside this function are not.
    for s in body {
        collect_fn_locals(s, sym, in_fn)?;
    }

    for s in body {
        check_stmt(s, sym, fns, Some(ret_ty))?;
    }

    // Non-void functions must end with a `return expr;`.
    match body.last() {
        Some(Stmt::Return(Some(_))) => Ok(()),
        Some(Stmt::Return(None)) => {
            if ret_ty.is_some() {
                Err(BfscError::Type(format!(
                    "function '{fn_name}' declares a return type but returns no value"
                )))
            } else {
                Ok(())
            }
        }
        _ => {
            if ret_ty.is_some() {
                Err(BfscError::Type(format!(
                    "function '{fn_name}' must end with `return <expr>;`"
                )))
            } else {
                Ok(())
            }
        }
    }
}

fn ensure_no_nested_return(stmt: &Stmt, fn_name: &str) -> Result<(), BfscError> {
    match stmt {
        Stmt::While { body, .. } => {
            for s in body {
                if matches!(s, Stmt::Return(_)) {
                    return Err(BfscError::Type(format!(
                        "in function '{fn_name}': `return` must be the last statement of the body (found inside a nested block)"
                    )));
                }
                ensure_no_nested_return(s, fn_name)?;
            }
        }
        Stmt::If { then_, else_, .. } => {
            for s in then_ {
                if matches!(s, Stmt::Return(_)) {
                    return Err(BfscError::Type(format!(
                        "in function '{fn_name}': `return` must be the last statement of the body (found inside a nested block)"
                    )));
                }
                ensure_no_nested_return(s, fn_name)?;
            }
            if let Some(eb) = else_ {
                for s in eb {
                    if matches!(s, Stmt::Return(_)) {
                        return Err(BfscError::Type(format!(
                            "in function '{fn_name}': `return` must be the last statement of the body (found inside a nested block)"
                        )));
                    }
                    ensure_no_nested_return(s, fn_name)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn collect_fn_locals(
    stmt: &Stmt,
    sym: &mut HashMap<String, TypeAnn>,
    in_fn: &mut HashSet<String>,
) -> Result<(), BfscError> {
    match stmt {
        Stmt::Let { name, ty, .. } => {
            if !in_fn.insert(name.clone()) {
                return Err(BfscError::Type(format!("duplicate variable '{name}'")));
            }
            sym.insert(name.clone(), ty.clone());
        }
        Stmt::While { body, .. } => {
            for s in body {
                collect_fn_locals(s, sym, in_fn)?;
            }
        }
        Stmt::If { then_, else_, .. } => {
            for s in then_ {
                collect_fn_locals(s, sym, in_fn)?;
            }
            if let Some(eb) = else_ {
                for s in eb {
                    collect_fn_locals(s, sym, in_fn)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// Type-check a single statement. `ret_ty_ctx = Some(rt)` means we're inside a
/// function body whose declared return type is `rt`; `None` means top-level
/// (where `return` is forbidden).
fn check_stmt(
    stmt: &Stmt,
    sym: &HashMap<String, TypeAnn>,
    fns: &FnTable,
    ret_ty_ctx: Option<Option<ScalarType>>,
) -> Result<(), BfscError> {
    match stmt {
        Stmt::Let { name, init, .. } => {
            if let Some(expr) = init {
                check_expr(expr, sym, fns)?;
                let _ = name;
            }
        }
        Stmt::Assign { lval, expr } => {
            check_lval(lval, sym, fns)?;
            check_expr(expr, sym, fns)?;
        }
        Stmt::While { cond, body } => {
            check_expr(cond, sym, fns)?;
            for s in body {
                check_stmt(s, sym, fns, ret_ty_ctx)?;
            }
        }
        Stmt::If { cond, then_, else_ } => {
            check_expr(cond, sym, fns)?;
            for s in then_ {
                check_stmt(s, sym, fns, ret_ty_ctx)?;
            }
            if let Some(eb) = else_ {
                for s in eb {
                    check_stmt(s, sym, fns, ret_ty_ctx)?;
                }
            }
        }
        Stmt::Scan(lval) => check_lval(lval, sym, fns)?,
        Stmt::Print(expr) | Stmt::Putchar(expr) => check_expr(expr, sym, fns)?,
        Stmt::Setpixel { x, y, color } => {
            check_expr(x, sym, fns)?;
            check_expr(y, sym, fns)?;
            check_expr(color, sym, fns)?;
        }
        Stmt::Getchar(lval) => check_lval(lval, sym, fns)?,
        Stmt::Call(name, args) => {
            check_call(name, args, sym, fns)?;
        }
        Stmt::Return(expr) => {
            let rt = match ret_ty_ctx {
                None => {
                    return Err(BfscError::Type(
                        "`return` not allowed at the top level".into(),
                    ));
                }
                Some(rt) => rt,
            };
            match (rt, expr) {
                (Some(_), Some(e)) => check_expr(e, sym, fns)?,
                (None, None) => {}
                (Some(_), None) => {
                    return Err(BfscError::Type(
                        "function declares a return type but `return;` has no value".into(),
                    ));
                }
                (None, Some(_)) => {
                    return Err(BfscError::Type(
                        "void function cannot return a value".into(),
                    ));
                }
            }
        }
    }
    Ok(())
}

fn check_lval(
    lval: &LValue,
    sym: &HashMap<String, TypeAnn>,
    fns: &FnTable,
) -> Result<(), BfscError> {
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
            check_expr(idx, sym, fns)?;
        }
    }
    Ok(())
}

fn check_expr(
    expr: &Expr,
    sym: &HashMap<String, TypeAnn>,
    fns: &FnTable,
) -> Result<(), BfscError> {
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
            check_expr(idx, sym, fns)?;
        }
        Expr::BinOp(_, l, r) => {
            check_expr(l, sym, fns)?;
            check_expr(r, sym, fns)?;
        }
        Expr::UnOp(_, e) => check_expr(e, sym, fns)?,
        Expr::Call(name, args) => {
            let sig = fns.get(name.as_str()).ok_or_else(|| {
                BfscError::Type(format!("call to undefined function '{name}'"))
            })?;
            if sig.ret_ty.is_none() {
                return Err(BfscError::Type(format!(
                    "void function '{name}' cannot be used in an expression"
                )));
            }
            check_call(name, args, sym, fns)?;
        }
    }
    Ok(())
}

fn check_call(
    name: &str,
    args: &[Expr],
    sym: &HashMap<String, TypeAnn>,
    fns: &FnTable,
) -> Result<(), BfscError> {
    let sig = fns
        .get(name)
        .ok_or_else(|| BfscError::Type(format!("call to undefined function '{name}'")))?
        .clone();
    if sig.params.len() != args.len() {
        return Err(BfscError::Type(format!(
            "function '{name}' expects {} argument(s), got {}",
            sig.params.len(),
            args.len()
        )));
    }
    for (i, (param, arg)) in sig.params.iter().zip(args.iter()).enumerate() {
        match &param.ty {
            TypeAnn::Scalar(_) => {
                // Scalar parameter: any scalar expression works; concrete
                // widths get reconciled by codegen's existing widening logic.
                check_expr(arg, sym, fns)?;
            }
            TypeAnn::Array(elem_ty, len) => {
                // Array parameter: argument must be a bare array identifier
                // with matching element type and length.
                let arr_name = match arg {
                    Expr::Var(n) => n,
                    _ => {
                        return Err(BfscError::Type(format!(
                            "argument {} of call to '{name}' must be an array name (array parameters are passed by reference)",
                            i + 1
                        )));
                    }
                };
                match sym.get(arr_name.as_str()) {
                    None => {
                        return Err(BfscError::Type(format!("undeclared variable '{arr_name}'")));
                    }
                    Some(TypeAnn::Scalar(_)) => {
                        return Err(BfscError::Type(format!(
                            "argument {} of call to '{name}' must be an array, but '{arr_name}' is a scalar",
                            i + 1
                        )));
                    }
                    Some(TypeAnn::Array(at, al)) => {
                        if at != elem_ty || al != len {
                            return Err(BfscError::Type(format!(
                                "argument {} of call to '{name}': expected [{}; {}] but got [{}; {}]",
                                i + 1,
                                elem_ty,
                                len,
                                at,
                                al
                            )));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn detect_recursion(fns: &[FnDef]) -> Result<(), BfscError> {
    // Build adjacency: caller → set of callees.
    let mut graph: HashMap<&str, Vec<&str>> = HashMap::new();
    for f in fns {
        let mut callees: Vec<&str> = Vec::new();
        collect_calls_in_body(&f.body, &mut callees);
        graph.insert(f.name.as_str(), callees);
    }

    // DFS with three-color marking.
    enum Color {
        White,
        Gray,
        Black,
    }
    let mut color: HashMap<&str, Color> = graph.keys().map(|k| (*k, Color::White)).collect();

    fn dfs<'a>(
        node: &'a str,
        graph: &HashMap<&'a str, Vec<&'a str>>,
        color: &mut HashMap<&'a str, Color>,
    ) -> Result<(), BfscError> {
        color.insert(node, Color::Gray);
        if let Some(succs) = graph.get(node) {
            for &succ in succs {
                match color.get(succ) {
                    Some(Color::Gray) => {
                        return Err(BfscError::Type(format!(
                            "recursive call detected involving '{succ}' — not supported"
                        )));
                    }
                    Some(Color::White) => dfs(succ, graph, color)?,
                    _ => {}
                }
            }
        }
        color.insert(node, Color::Black);
        Ok(())
    }

    let names: Vec<&str> = graph.keys().copied().collect();
    for n in names {
        if matches!(color.get(n), Some(Color::White)) {
            dfs(n, &graph, &mut color)?;
        }
    }
    Ok(())
}

fn collect_calls_in_body<'a>(body: &'a [Stmt], out: &mut Vec<&'a str>) {
    for s in body {
        collect_calls_in_stmt(s, out);
    }
}

fn collect_calls_in_stmt<'a>(stmt: &'a Stmt, out: &mut Vec<&'a str>) {
    match stmt {
        Stmt::Let { init, .. } => {
            if let Some(e) = init {
                collect_calls_in_expr(e, out);
            }
        }
        Stmt::Assign { lval, expr } => {
            if let LValue::Index(_, idx) = lval {
                collect_calls_in_expr(idx, out);
            }
            collect_calls_in_expr(expr, out);
        }
        Stmt::While { cond, body } => {
            collect_calls_in_expr(cond, out);
            collect_calls_in_body(body, out);
        }
        Stmt::If { cond, then_, else_ } => {
            collect_calls_in_expr(cond, out);
            collect_calls_in_body(then_, out);
            if let Some(eb) = else_ {
                collect_calls_in_body(eb, out);
            }
        }
        Stmt::Scan(lval) | Stmt::Getchar(lval) => {
            if let LValue::Index(_, idx) = lval {
                collect_calls_in_expr(idx, out);
            }
        }
        Stmt::Print(e) | Stmt::Putchar(e) => collect_calls_in_expr(e, out),
        Stmt::Setpixel { x, y, color } => {
            collect_calls_in_expr(x, out);
            collect_calls_in_expr(y, out);
            collect_calls_in_expr(color, out);
        }
        Stmt::Call(name, args) => {
            out.push(name.as_str());
            for a in args {
                collect_calls_in_expr(a, out);
            }
        }
        Stmt::Return(Some(e)) => collect_calls_in_expr(e, out),
        Stmt::Return(None) => {}
    }
}

fn collect_calls_in_expr<'a>(expr: &'a Expr, out: &mut Vec<&'a str>) {
    match expr {
        Expr::Int(_) | Expr::Var(_) => {}
        Expr::Index(_, idx) => collect_calls_in_expr(idx, out),
        Expr::BinOp(_, l, r) => {
            collect_calls_in_expr(l, out);
            collect_calls_in_expr(r, out);
        }
        Expr::UnOp(_, e) => collect_calls_in_expr(e, out),
        Expr::Call(name, args) => {
            out.push(name.as_str());
            for a in args {
                collect_calls_in_expr(a, out);
            }
        }
    }
}
