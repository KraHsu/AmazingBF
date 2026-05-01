//! BFS-to-Brainfuck code emitter.
//!
//! Walks the type-checked AST and emits a BF source string in a single pass.
//! `BfEmitter` maintains the current tape pointer, a bitmap of live temp cells
//! above `MemMap::temp_base`, and a library of n-byte primitives (arithmetic,
//! comparison, I/O) so higher-level statements compose without reimplementing
//! BF idioms. Output is deterministic: identical input yields identical BF.

use super::ast::*;
use super::layout::{CellLayout, MemMap};
use super::typeck::FnTable;
use std::collections::{BTreeSet, HashMap};

/// Sentinel name used to address the enclosing function's return slot when
/// emitting `Stmt::Return(Some(_))`. The name contains a non-identifier
/// character so it cannot collide with any user-declared BFS binding.
const RET_SLOT: &str = "$ret";

/// Build the program's function definition table, keyed by function name,
/// from the parsed AST.
fn fn_defs(program: &Program) -> HashMap<String, FnDef> {
    program
        .fns
        .iter()
        .map(|f| (f.name.clone(), f.clone()))
        .collect()
}

/// Emit a Brainfuck source string for the type-checked BFS program against
/// the frozen top-level memory layout and function table produced by `typeck`.
pub(crate) fn emit(program: &Program, layout: &MemMap, _fns: &FnTable) -> String {
    let defs = fn_defs(program);
    let mut emitter = BfEmitter::new(layout, &defs);
    for s in &program.top {
        emitter.gen_stmt(s);
    }
    emitter.output
}

struct BfEmitter<'a> {
    output: String,
    ptr: usize,
    layout: &'a MemMap,
    fns: &'a HashMap<String, FnDef>,
    /// Stack of per-call-frame binding overlays (parameters, return slot, and
    /// function-local `let` bindings). Top-level code runs with an empty
    /// `scopes` stack so `lookup` falls through to the static `layout`.
    scopes: Vec<HashMap<String, CellLayout>>,
    used: BTreeSet<usize>,
}

impl<'a> BfEmitter<'a> {
    fn new(layout: &'a MemMap, fns: &'a HashMap<String, FnDef>) -> Self {
        BfEmitter {
            output: String::new(),
            ptr: 0,
            layout,
            fns,
            scopes: Vec::new(),
            used: BTreeSet::new(),
        }
    }

    /// Look up a binding by name, preferring the innermost active scope and
    /// falling back to the static top-level layout. Panics on unknown names
    /// because typeck has already validated them.
    fn lookup(&self, name: &str) -> CellLayout {
        for scope in self.scopes.iter().rev() {
            if let Some(l) = scope.get(name) {
                return l.clone();
            }
        }
        self.layout.get(name).unwrap().clone()
    }

    fn in_fn_scope(&self) -> bool {
        !self.scopes.is_empty()
    }

    fn temp_base(&self) -> usize {
        self.layout.temp_base
    }

    // Temp allocator.

    fn talloc(&mut self) -> usize {
        let mut i = 0;
        while self.used.contains(&i) {
            i += 1;
        }
        self.used.insert(i);
        self.temp_base() + i
    }

    fn tfree(&mut self, cell: usize) {
        self.used.remove(&cell.saturating_sub(self.temp_base()));
    }

    fn talloc_n(&mut self, n: usize) -> usize {
        let mut i = 0;
        loop {
            let ok = (0..n).all(|k| !self.used.contains(&(i + k)));
            if ok {
                for k in 0..n {
                    self.used.insert(i + k);
                }
                return self.temp_base() + i;
            }
            i += 1;
        }
    }

    fn tfree_n(&mut self, base: usize, n: usize) {
        let start = base.saturating_sub(self.temp_base());
        for k in 0..n {
            self.used.remove(&(start + k));
        }
    }

    fn tsave(&self) -> BTreeSet<usize> {
        self.used.clone()
    }
    fn trestore(&mut self, s: BTreeSet<usize>) {
        self.used = s;
    }

    // BF emission primitives.

    fn raw(&mut self, s: &str) {
        self.output.push_str(s);
    }

    fn goto(&mut self, c: usize) {
        if c > self.ptr {
            for _ in 0..(c - self.ptr) {
                self.output.push('>');
            }
        } else if c < self.ptr {
            for _ in 0..(self.ptr - c) {
                self.output.push('<');
            }
        }
        self.ptr = c;
    }

    fn clear(&mut self, c: usize) {
        self.goto(c);
        self.raw("[-]");
    }
    fn inc(&mut self, c: usize, n: usize) {
        if n > 0 {
            self.goto(c);
            for _ in 0..n {
                self.output.push('+');
            }
        }
    }
    fn dec(&mut self, c: usize, n: usize) {
        if n > 0 {
            self.goto(c);
            for _ in 0..n {
                self.output.push('-');
            }
        }
    }

    fn setv(&mut self, c: usize, v: u8) {
        self.clear(c);
        if v > 0 {
            self.inc(c, v as usize);
        }
    }

    fn set_const_n(&mut self, base: usize, val: u32, n: usize) {
        for i in 0..n {
            self.setv(base + i, (val >> (i * 8)) as u8);
        }
    }

    fn inp(&mut self, c: usize) {
        self.goto(c);
        self.output.push(',');
    }
    fn out(&mut self, c: usize) {
        self.goto(c);
        self.output.push('.');
    }

    // non-destructive copy src → dst (uses one internal temp)
    fn copy(&mut self, src: usize, dst: usize) {
        let tmp = self.talloc();
        self.clear(dst);
        self.clear(tmp);
        self.goto(src);
        self.raw("[");
        self.goto(dst);
        self.output.push('+');
        self.goto(tmp);
        self.output.push('+');
        self.goto(src);
        self.raw("-]");
        self.goto(tmp);
        self.raw("[");
        self.goto(src);
        self.output.push('+');
        self.goto(tmp);
        self.raw("-]");
        self.tfree(tmp);
    }

    fn copy_n(&mut self, src: usize, dst: usize, n: usize) {
        for i in 0..n {
            self.copy(src + i, dst + i);
        }
    }

    // destructive move src → dst (src becomes 0)
    fn bfmove(&mut self, src: usize, dst: usize) {
        self.clear(dst);
        self.goto(src);
        self.raw("[");
        self.goto(dst);
        self.output.push('+');
        self.goto(src);
        self.raw("-]");
    }

    fn bfmove_n(&mut self, src: usize, dst: usize, n: usize) {
        for i in 0..n {
            self.bfmove(src + i, dst + i);
        }
    }

    // dst += src (src becomes 0); single byte
    fn add_to(&mut self, src: usize, dst: usize) {
        self.goto(src);
        self.raw("[");
        self.goto(dst);
        self.output.push('+');
        self.goto(src);
        self.raw("-]");
    }

    // dst -= src (src becomes 0); single byte
    fn sub_from(&mut self, src: usize, dst: usize) {
        self.goto(src);
        self.raw("[");
        self.goto(dst);
        self.output.push('-');
        self.goto(src);
        self.raw("-]");
    }

    // in-place: c = (c != 0) ? 1 : 0; returns same cell
    fn is_nz(&mut self, cell: usize) -> usize {
        let r = self.talloc();
        self.clear(r);
        let tc = self.talloc();
        self.copy(cell, tc);
        self.goto(tc);
        self.raw("[");
        self.setv(r, 1);
        self.clear(tc);
        self.goto(tc);
        self.raw("]");
        self.tfree(tc);
        r
    }

    // in-place negate: c = (c == 0) ? 1 : 0
    fn negate(&mut self, c: usize) -> usize {
        let t = self.talloc();
        self.bfmove(c, t);
        self.inc(c, 1);
        self.goto(t);
        self.raw("[");
        self.clear(c);
        self.clear(t);
        self.goto(t);
        self.raw("]");
        self.tfree(t);
        c
    }

    // if_else: cond is consumed
    fn if_else<F, G>(&mut self, cond: usize, then_fn: F, else_fn: Option<G>)
    where
        F: FnOnce(&mut Self),
        G: FnOnce(&mut Self),
    {
        match else_fn {
            None => {
                self.goto(cond);
                self.raw("[");
                then_fn(self);
                self.clear(cond);
                self.goto(cond);
                self.raw("]");
            }
            Some(ef_fn) => {
                let ef = self.talloc();
                self.setv(ef, 1);
                self.goto(cond);
                self.raw("[");
                self.clear(ef);
                then_fn(self);
                self.clear(cond);
                self.goto(cond);
                self.raw("]");
                self.goto(ef);
                self.raw("[");
                ef_fn(self);
                self.clear(ef);
                self.goto(ef);
                self.raw("]");
                self.tfree(ef);
            }
        }
    }

    // zero-extend: move from_w bytes at src into to_w-byte block (to_w >= from_w)
    // src (from_w) is freed; returns new to_w base
    fn widen(&mut self, src: usize, from_w: usize, to_w: usize) -> usize {
        if from_w == to_w {
            return src;
        }
        let t = self.talloc_n(to_w);
        self.bfmove_n(src, t, from_w);
        for i in from_w..to_w {
            self.clear(t + i);
        }
        self.tfree_n(src, from_w);
        t
    }

    // Single-byte comparisons (a, b: consumed temps; returns new temp).

    fn cmp_gt(&mut self, a: usize, b: usize) -> usize {
        let result = self.talloc();
        self.clear(result);
        let flag = self.talloc();
        self.setv(flag, 1);
        self.goto(flag);
        self.raw("[");

        let bnz = self.is_nz(b);
        self.goto(bnz);
        self.raw("["); // if b != 0
        self.dec(b, 1);
        let anz = self.is_nz(a);
        self.goto(anz);
        self.raw("[");
        self.dec(a, 1);
        self.clear(anz);
        self.goto(anz);
        self.raw("]");
        // else branch: a was 0 → a <= b, clear flag
        let ef_a = self.talloc();
        self.setv(ef_a, 1);
        let anz2 = self.is_nz(a);
        self.goto(anz2);
        self.raw("[");
        self.clear(ef_a);
        self.clear(anz2);
        self.goto(anz2);
        self.raw("]");
        self.tfree(anz2);
        self.goto(ef_a);
        self.raw("[");
        self.clear(flag);
        self.clear(ef_a);
        self.goto(ef_a);
        self.raw("]");
        self.tfree(ef_a);
        self.clear(bnz);
        self.goto(bnz);
        self.raw("]");

        // else branch: b == 0
        let ef_b = self.talloc();
        self.setv(ef_b, 1);
        let bnz2 = self.is_nz(b);
        self.goto(bnz2);
        self.raw("[");
        self.clear(ef_b);
        self.clear(bnz2);
        self.goto(bnz2);
        self.raw("]");
        self.tfree(bnz2);
        self.goto(ef_b);
        self.raw("[");
        let anz3 = self.is_nz(a);
        self.goto(anz3);
        self.raw("[");
        self.setv(result, 1);
        self.clear(anz3);
        self.goto(anz3);
        self.raw("]");
        self.tfree(anz3);
        self.clear(flag);
        self.clear(ef_b);
        self.goto(ef_b);
        self.raw("]");
        self.tfree(ef_b);
        self.tfree(bnz);

        self.goto(flag);
        self.raw("]");
        self.tfree(flag);
        self.tfree(a);
        self.tfree(b);
        result
    }

    fn cmp_lt(&mut self, a: usize, b: usize) -> usize {
        self.cmp_gt(b, a)
    }
    fn cmp_eq(&mut self, a: usize, b: usize) -> usize {
        self.sub_from(b, a);
        self.tfree(b);
        self.negate(a)
    }

    // Multi-byte comparisons (n >= 1; a, b: consumed n-cell temps; returns 1-cell temp).

    // a < b unsigned little-endian n bytes
    fn cmp_lt_n(&mut self, a: usize, b: usize, n: usize) -> usize {
        if n == 1 {
            return self.cmp_lt(a, b);
        }
        // Accumulate from LSB up: result = (a[i] < b[i]) || (a[i] == b[i] && result_from_lower)
        let result = self.talloc();
        self.clear(result);
        for i in 0..n {
            let a1 = self.talloc();
            self.copy(a + i, a1);
            let b1 = self.talloc();
            self.copy(b + i, b1);
            let a2 = self.talloc();
            self.copy(a + i, a2);
            let b2 = self.talloc();
            self.copy(b + i, b2);
            let lt = self.cmp_lt(a1, b1);
            let eq = self.cmp_eq(a2, b2);

            let new_result = self.talloc();
            self.clear(new_result);
            let prev_copy = self.talloc();
            self.copy(result, prev_copy);
            self.clear(result);

            // new_result = lt || (eq && prev_copy)
            let lt2 = lt;
            let nr = new_result;
            self.goto(lt2);
            self.raw("[");
            self.setv(nr, 1);
            self.clear(lt2);
            self.goto(lt2);
            self.raw("]");
            self.tfree(lt2);

            let eq2 = eq;
            let pc = prev_copy;
            let nr2 = new_result;
            self.goto(eq2);
            self.raw("[");
            self.goto(pc);
            self.raw("[");
            self.setv(nr2, 1);
            self.clear(pc);
            self.goto(pc);
            self.raw("]");
            self.clear(eq2);
            self.goto(eq2);
            self.raw("]");
            self.tfree(eq2);
            self.tfree(prev_copy);

            self.bfmove(new_result, result);
            self.tfree(new_result);
        }
        self.tfree_n(a, n);
        self.tfree_n(b, n);
        result
    }

    fn cmp_gt_n(&mut self, a: usize, b: usize, n: usize) -> usize {
        self.cmp_lt_n(b, a, n)
    }
    fn cmp_le_n(&mut self, a: usize, b: usize, n: usize) -> usize {
        let r = self.cmp_gt_n(a, b, n);
        self.negate(r)
    }
    fn cmp_ge_n(&mut self, a: usize, b: usize, n: usize) -> usize {
        let r = self.cmp_lt_n(a, b, n);
        self.negate(r)
    }

    fn cmp_eq_n(&mut self, a: usize, b: usize, n: usize) -> usize {
        if n == 1 {
            return self.cmp_eq(a, b);
        }
        // all bytes equal?
        let result = self.talloc();
        self.setv(result, 1);
        for i in 0..n {
            let ai = self.talloc();
            self.copy(a + i, ai);
            let bi = self.talloc();
            self.copy(b + i, bi);
            let byte_eq = self.cmp_eq(ai, bi);
            // result = result && byte_eq
            let old = self.talloc();
            self.bfmove(result, old);
            self.clear(result);
            self.goto(old);
            self.raw("[");
            self.goto(byte_eq);
            self.raw("[");
            self.setv(result, 1);
            self.clear(byte_eq);
            self.goto(byte_eq);
            self.raw("]");
            self.clear(old);
            self.goto(old);
            self.raw("]");
            self.tfree(old);
            self.tfree(byte_eq);
        }
        self.tfree_n(a, n);
        self.tfree_n(b, n);
        result
    }

    fn cmp_ne_n(&mut self, a: usize, b: usize, n: usize) -> usize {
        let r = self.cmp_eq_n(a, b, n);
        self.negate(r)
    }

    // Multi-byte arithmetic.

    // dst[0..n] += src[0..n], carry-aware, wraps at 2^(8*n). src freed.
    fn add_n(&mut self, dst: usize, src: usize, n: usize) {
        if n == 1 {
            self.add_to(src, dst);
            self.tfree(src);
            return;
        }
        for i in 0..n {
            let orig = self.talloc();
            self.copy(dst + i, orig);
            self.add_to(src + i, dst + i);
            if i < n - 1 {
                let result_copy = self.talloc();
                self.copy(dst + i, result_copy);
                // carry = orig > result (wrapped)
                let carry = self.cmp_gt(orig, result_copy);
                self.add_to(carry, dst + i + 1);
                self.tfree(carry);
            } else {
                self.tfree(orig);
            }
        }
        self.tfree_n(src, n);
    }

    // dst[0..n] -= src[0..n], borrow-aware, wraps at 2^(8*n). src freed.
    fn sub_n(&mut self, dst: usize, src: usize, n: usize) {
        if n == 1 {
            self.sub_from(src, dst);
            self.tfree(src);
            return;
        }
        for i in 0..n {
            let orig = self.talloc();
            self.copy(dst + i, orig);
            self.sub_from(src + i, dst + i);
            if i < n - 1 {
                let result_copy = self.talloc();
                self.copy(dst + i, result_copy);
                // borrow = result > orig (wrapped down)
                let borrow = self.cmp_gt(result_copy, orig);
                let tgt = dst + i + 1;
                self.if_else(
                    borrow,
                    move |bf| {
                        bf.dec(tgt, 1);
                    },
                    None::<fn(&mut Self)>,
                );
            } else {
                self.tfree(orig);
            }
        }
        self.tfree_n(src, n);
    }

    // Increment n-byte little-endian value by 1 (in-place)
    fn inc_n(&mut self, target: usize, n: usize) {
        let one = self.talloc_n(n);
        self.set_const_n(one, 1, n);
        self.add_n(target, one, n);
    }

    // Decrement n-byte little-endian value by 1 (in-place)
    fn dec_n(&mut self, target: usize, n: usize) {
        let one = self.talloc_n(n);
        self.set_const_n(one, 1, n);
        self.sub_n(target, one, n);
    }

    // is_nz for n-byte: nonzero if any byte nonzero
    fn is_nz_n(&mut self, base: usize, n: usize) -> usize {
        if n == 1 {
            return self.is_nz(base);
        }
        let r = self.talloc();
        self.clear(r);
        for i in 0..n {
            let bc = self.talloc();
            self.copy(base + i, bc);
            self.goto(bc);
            self.raw("[");
            self.setv(r, 1);
            self.clear(bc);
            self.goto(bc);
            self.raw("]");
            self.tfree(bc);
        }
        r
    }

    // a * b n-byte (repeated add); a, b consumed; returns n-byte result
    fn arith_mul_n(&mut self, a: usize, b: usize, n: usize) -> usize {
        if n == 1 {
            return self.arith_mul(a, b);
        }
        let r = self.talloc_n(n);
        for i in 0..n {
            self.clear(r + i);
        }
        // while a != 0: a -= 1; r += b_copy
        let flag = self.is_nz_n(a, n);
        self.goto(flag);
        self.raw("[");
        self.clear(flag);
        self.dec_n(a, n);
        let bc = self.talloc_n(n);
        self.copy_n(b, bc, n);
        self.add_n(r, bc, n);
        let flag2 = self.is_nz_n(a, n);
        self.bfmove(flag2, flag);
        self.tfree(flag2);
        self.goto(flag);
        self.raw("]");
        self.tfree(flag);
        self.tfree_n(a, n);
        self.tfree_n(b, n);
        r
    }

    // a / b n-byte (repeated sub); a, b consumed; returns quotient n-byte
    fn arith_div_n(&mut self, a: usize, b: usize, n: usize) -> usize {
        if n == 1 {
            return self.arith_div(a, b);
        }
        let q = self.talloc_n(n);
        for i in 0..n {
            self.clear(q + i);
        }
        let flag = self.talloc();
        self.setv(flag, 1);
        self.goto(flag);
        self.raw("[");
        self.clear(flag);
        let ta = self.talloc_n(n);
        let tb = self.talloc_n(n);
        self.copy_n(a, ta, n);
        self.copy_n(b, tb, n);
        let bgt = self.cmp_gt_n(tb, ta, n); // b > a?
        let ge = self.negate(bgt); // a >= b
        let b2 = b;
        let a2 = a;
        let q2 = q;
        let flag2 = flag;
        let n2 = n;
        self.goto(ge);
        self.raw("[");
        let ts = self.talloc_n(n2);
        self.copy_n(b2, ts, n2);
        self.sub_n(a2, ts, n2);
        self.inc_n(q2, n2);
        self.setv(flag2, 1);
        self.clear(ge);
        self.goto(ge);
        self.raw("]");
        self.tfree(ge);
        self.goto(flag);
        self.raw("]");
        self.tfree(flag);
        self.tfree_n(a, n);
        self.tfree_n(b, n);
        q
    }

    // a % b n-byte; a, b consumed; returns remainder in a
    fn arith_mod_n(&mut self, a: usize, b: usize, n: usize) -> usize {
        if n == 1 {
            return self.arith_mod(a, b);
        }
        let flag = self.talloc();
        self.setv(flag, 1);
        self.goto(flag);
        self.raw("[");
        self.clear(flag);
        let ta = self.talloc_n(n);
        let tb = self.talloc_n(n);
        self.copy_n(a, ta, n);
        self.copy_n(b, tb, n);
        let bgt = self.cmp_gt_n(tb, ta, n);
        let ge = self.negate(bgt);
        let b2 = b;
        let a2 = a;
        let flag2 = flag;
        let n2 = n;
        self.goto(ge);
        self.raw("[");
        let ts = self.talloc_n(n2);
        self.copy_n(b2, ts, n2);
        self.sub_n(a2, ts, n2);
        self.setv(flag2, 1);
        self.clear(ge);
        self.goto(ge);
        self.raw("]");
        self.tfree(ge);
        self.goto(flag);
        self.raw("]");
        self.tfree(flag);
        self.tfree_n(b, n);
        a
    }

    // Single-byte arithmetic (legacy, n=1 fast path).

    fn arith_mul(&mut self, a: usize, b: usize) -> usize {
        let r = self.talloc();
        self.clear(r);
        let tc = self.talloc();
        self.goto(a);
        self.raw("[");
        self.dec(a, 1);
        self.copy(b, tc);
        self.add_to(tc, r);
        self.goto(a);
        self.raw("]");
        self.tfree(tc);
        self.tfree(a);
        self.tfree(b);
        r
    }

    fn arith_div(&mut self, a: usize, b: usize) -> usize {
        let q = self.talloc();
        self.clear(q);
        let flag = self.talloc();
        self.setv(flag, 1);
        self.goto(flag);
        self.raw("[");
        self.clear(flag);
        let ta = self.talloc();
        let tb = self.talloc();
        self.copy(a, ta);
        self.copy(b, tb);
        let bga = self.cmp_gt(tb, ta);
        let ge = self.negate(bga);
        let b2 = b;
        let a2 = a;
        let q2 = q;
        let flag2 = flag;
        self.goto(ge);
        self.raw("[");
        let ts = self.talloc();
        self.copy(b2, ts);
        self.sub_from(ts, a2);
        self.tfree(ts);
        self.inc(q2, 1);
        self.setv(flag2, 1);
        self.clear(ge);
        self.goto(ge);
        self.raw("]");
        self.tfree(ge);
        self.goto(flag);
        self.raw("]");
        self.tfree(flag);
        self.tfree(a);
        self.tfree(b);
        q
    }

    fn arith_mod(&mut self, a: usize, b: usize) -> usize {
        let flag = self.talloc();
        self.setv(flag, 1);
        self.goto(flag);
        self.raw("[");
        self.clear(flag);
        let ta = self.talloc();
        let tb = self.talloc();
        self.copy(a, ta);
        self.copy(b, tb);
        let bga = self.cmp_gt(tb, ta);
        let ge = self.negate(bga);
        let b2 = b;
        let a2 = a;
        let flag2 = flag;
        self.goto(ge);
        self.raw("[");
        let ts = self.talloc();
        self.copy(b2, ts);
        self.sub_from(ts, a2);
        self.tfree(ts);
        self.setv(flag2, 1);
        self.clear(ge);
        self.goto(ge);
        self.raw("]");
        self.tfree(ge);
        self.goto(flag);
        self.raw("]");
        self.tfree(flag);
        self.tfree(b);
        a
    }

    // Array access.

    fn arr_read(&mut self, layout: &super::layout::CellLayout, idx_expr: &Expr) -> (usize, usize) {
        // Walk-storage arrays (typeck/layout sets this for u8 arrays bigger
        // than 256 elements) use a moving-pointer idiom that emits O(1) BF
        // per access regardless of `arr_len`. The legacy linear-scan paths
        // below stay in place for ≤256-cell arrays — they're byte-identical
        // to the older emitter on existing fixtures.
        //
        // Single-chunk arrays go through the fast-path `arr_read_walk`;
        // anything bigger uses the chunk-level outer walk so per-access
        // emit stays constant in `num_chunks`.
        if let super::layout::ArrayLayout::Walk {
            chunk_size,
            num_chunks,
        } = layout.layout
        {
            if num_chunks == 1 {
                return self.arr_read_walk(layout, idx_expr, chunk_size);
            }
            return self.arr_read_outer_walk(layout, idx_expr, chunk_size, num_chunks);
        }
        let base = layout.base;
        let arr_len = layout.array_len();
        let ew = layout.elem_width();
        // Index width: 1 byte for arrays ≤256 (back-compat, byte-identical BF),
        // 2 bytes when arr_len exceeds u8 range.
        let iw = if arr_len > 256 { 2 } else { 1 };
        let result = self.talloc_n(ew);
        for k in 0..ew {
            self.clear(result + k);
        }
        if iw == 1 {
            // Legacy 1-byte path. Re-copies idx and decrements by `i` each
            // iteration (O(arr_len^2) emitted BF chars, fine for ≤256).
            let idx = self.eval_expr_1(idx_expr);
            for i in 0..arr_len {
                let ti = self.talloc();
                self.copy(idx, ti);
                if i > 0 {
                    self.dec(ti, i);
                }
                let eq = self.talloc();
                self.setv(eq, 1);
                let tc = self.talloc();
                self.copy(ti, tc);
                self.goto(tc);
                self.raw("[");
                self.clear(eq);
                self.clear(tc);
                self.goto(tc);
                self.raw("]");
                self.tfree(tc);
                let base_i = base + i * ew;
                let r = result;
                let ew2 = ew;
                self.goto(eq);
                self.raw("[");
                for off in 0..ew2 {
                    self.copy(base_i + off, r + off);
                }
                self.clear(eq);
                self.goto(eq);
                self.raw("]");
                self.tfree(eq);
                self.tfree(ti);
            }
            self.tfree(idx);
        } else {
            // 2-byte path. Hoist `ti` and decrement by 1 per iteration so the
            // emitted BF stays linear in arr_len rather than quadratic.
            let idx = self.eval_expr_w(idx_expr, iw);
            let ti = self.talloc_n(iw);
            self.copy_n(idx, ti, iw);
            for i in 0..arr_len {
                if i > 0 {
                    self.dec_n(ti, iw);
                }
                let nz = self.is_nz_n(ti, iw);
                let eq = self.negate(nz);
                let base_i = base + i * ew;
                self.goto(eq);
                self.raw("[");
                for off in 0..ew {
                    self.copy(base_i + off, result + off);
                }
                self.clear(eq);
                self.goto(eq);
                self.raw("]");
                self.tfree(eq);
            }
            // ti was decremented to (idx - (arr_len-1)); is_nz_n / negate left
            // it intact. Clear it before freeing.
            for k in 0..iw {
                self.clear(ti + k);
            }
            self.tfree_n(ti, iw);
            self.tfree_n(idx, iw);
        }
        (result, ew)
    }

    fn arr_write(
        &mut self,
        layout: &super::layout::CellLayout,
        idx_expr: &Expr,
        val: usize,
        val_w: usize,
    ) {
        // See arr_read above for the same dispatch — walk storage handles
        // the giant-array case here too. `val_w` may be wider than the
        // element width on this path; truncate to the low byte before
        // handing it to the walk emitter (the BFS source pre-checks via
        // typeck so this is just defensive).
        if let super::layout::ArrayLayout::Walk {
            chunk_size,
            num_chunks,
        } = layout.layout
        {
            // Walk storage is u8-only; truncate val to the low byte if
            // the caller handed us a wider expression result.
            let v_byte = if val_w > 1 {
                let lo = self.talloc();
                self.bfmove(val, lo);
                for i in 1..val_w {
                    self.clear(val + i);
                }
                self.tfree_n(val, val_w);
                lo
            } else {
                val
            };
            if num_chunks == 1 {
                self.arr_write_walk(layout, idx_expr, v_byte, chunk_size);
            } else {
                self.arr_write_outer_walk(layout, idx_expr, v_byte, chunk_size, num_chunks);
            }
            return;
        }
        let base = layout.base;
        let arr_len = layout.array_len();
        let ew = layout.elem_width();
        // Widen or truncate val to match element width
        let val = if val_w < ew {
            self.widen(val, val_w, ew)
        } else {
            val
        };
        // Index width matches arr_read.
        let iw = if arr_len > 256 { 2 } else { 1 };
        if iw == 1 {
            let idx = self.eval_expr_1(idx_expr);
            for i in 0..arr_len {
                let ti = self.talloc();
                self.copy(idx, ti);
                if i > 0 {
                    self.dec(ti, i);
                }
                let eq = self.talloc();
                self.setv(eq, 1);
                let tc = self.talloc();
                self.copy(ti, tc);
                self.goto(tc);
                self.raw("[");
                self.clear(eq);
                self.clear(tc);
                self.goto(tc);
                self.raw("]");
                self.tfree(tc);
                let base_i = base + i * ew;
                let v2 = val;
                let ew2 = ew;
                self.goto(eq);
                self.raw("[");
                for off in 0..ew2 {
                    self.clear(base_i + off);
                    self.copy(v2 + off, base_i + off);
                }
                self.clear(eq);
                self.goto(eq);
                self.raw("]");
                self.tfree(eq);
                self.tfree(ti);
            }
            self.tfree(idx);
        } else {
            let idx = self.eval_expr_w(idx_expr, iw);
            let ti = self.talloc_n(iw);
            self.copy_n(idx, ti, iw);
            for i in 0..arr_len {
                if i > 0 {
                    self.dec_n(ti, iw);
                }
                let nz = self.is_nz_n(ti, iw);
                let eq = self.negate(nz);
                let base_i = base + i * ew;
                self.goto(eq);
                self.raw("[");
                for off in 0..ew {
                    self.clear(base_i + off);
                    self.copy(val + off, base_i + off);
                }
                self.clear(eq);
                self.goto(eq);
                self.raw("]");
                self.tfree(eq);
            }
            for k in 0..iw {
                self.clear(ti + k);
            }
            self.tfree_n(ti, iw);
            self.tfree_n(idx, iw);
        }
        self.tfree_n(val, ew);
    }

    // Walk-based array access (moving-pointer idiom).
    //
    // Layout (per chunk, written `[..]` per cell):
    //   [P_a, P_b, P_c, V_0, S_0a, S_0b, S_0c, V_1, S_1a, S_1b, S_1c, …
    //    V_(N-1), S_(N-1)a, S_(N-1)b, S_(N-1)c, E]
    // where `N = chunk_size`, each "slot" k after the 3-cell prefix is
    // `(V_k, S_ka, S_kb, S_kc)` (4 cells), and a trailing scratch cell E
    // (offset `4*N+3`) is reserved for the outer walk to park its
    // chunk-id survivor across the inner walk's body. The inner walk
    // never touches E.
    //
    // The walk transfers `idx` through the S_a control cells in strides
    // of 4, dropping a survivor (S_b) and a value carrier (S_c) at the
    // target slot. Forward + read step + backward walk run with O(1)
    // emitted BF — the only thing that grows with `arr_len` is runtime
    // (each access takes O(idx) tape ops).
    //
    // For `arr_len > chunk_size` the array is broken into `num_chunks`
    // independent chunks. The chunk-level dispatch lives in
    // `arr_read_outer_walk` / `arr_write_outer_walk`: it walks chunks
    // with the same moving-pointer idiom (3-cell prefix transfer at
    // stride `chunk_total`), so per-access emit stays O(chunk_total)
    // regardless of `num_chunks`.
    //
    // Setup invariant: P_a, P_b, P_c, E are 0 between accesses (every
    // walk restores them). Layout zeroes the storage at allocation time
    // and the BFS source never initialises walk-typed arrays directly.

    /// Single-chunk fast path. Caller has already established `num_chunks
    /// == 1`; this routine assumes the chunk lives at `layout.base` and
    /// leaves `self.ptr` at `base + 2` (P_c, now zero).
    fn arr_read_walk(
        &mut self,
        layout: &super::layout::CellLayout,
        idx_expr: &Expr,
        chunk_size: usize,
    ) -> (usize, usize) {
        debug_assert_eq!(layout.elem_width(), 1, "walk storage is u8-only");
        debug_assert_eq!(chunk_size, 256, "walk chunk_size must be 256 for u8 counter");

        let result = self.talloc();
        self.clear(result);

        let idx = self.eval_expr_w(idx_expr, 1);
        self.walk_read_chunk(layout.base, idx, result);
        (result, 1)
    }

    fn arr_write_walk(
        &mut self,
        layout: &super::layout::CellLayout,
        idx_expr: &Expr,
        val: usize,
        chunk_size: usize,
    ) {
        debug_assert_eq!(layout.elem_width(), 1, "walk storage is u8-only");
        debug_assert_eq!(chunk_size, 256);

        let idx = self.eval_expr_w(idx_expr, 1);
        self.walk_write_chunk(layout.base, idx, val);
    }

    // Walk a single chunk to read `chunk_base[local_offset]` into `result`.
    // Consumes `local_offset` (moved into the chunk's P_a). On entry, the
    // chunk's P_a/P_b/P_c may carry stale state; the routine clears them
    // first. On exit, ptr is at `chunk_base + 2` (P_c, now zero).
    fn walk_read_chunk(&mut self, chunk_base: usize, local_offset: usize, result: usize) {
        // Defensive clears (walks always restore, but freshly allocated
        // chunk storage isn't guaranteed to be 0 if prior runs were
        // interrupted).
        self.clear(chunk_base);
        self.clear(chunk_base + 1);
        self.clear(chunk_base + 2);

        // Move local_offset → P_a (destructive).
        self.goto(local_offset);
        self.raw("[-");
        self.goto(chunk_base);
        self.raw("+");
        self.goto(local_offset);
        self.raw("]");
        self.tfree(local_offset);

        self.goto(chunk_base);
        // The walk's invariant is "cursor at P_a, P_a == idx, P_b == P_c == 0".
        // Build P_b = idx via P_c, then run forward walk → read step →
        // backward walk. After backward walk the cursor is at P_b.
        // 1) Setup P_b = idx (via P_c).
        self.raw("[->+>+<<]>>[-<<+>>]<<");
        // 2) Forward walk (control + survivor only — value cell is for
        //    write paths). After: cursor at S_(idx-1)_a (offset 4*idx).
        self.raw("[-[->>>>+<<<<]>[->>>>+<<<<]<>>>>]");
        // 3) Step to V_idx.
        self.raw(">>>");
        // 4) Read step: clear S_(idx-1)_c (or P_c when idx=0), copy V_idx
        //    into it (and into S_idx_a), restore V_idx from S_idx_a.
        self.raw("<[-]>[-<+>>+<]>[-<+>]<");
        // 5) Step back to S_(idx-1)_b (cursor at offset 4*idx + 1).
        self.raw("<<");
        // 6) Backward walk (counter + value carrier). After: cursor at
        //    P_b. P_c now holds the read value.
        self.raw("[-[-<<<<+>>>>]>[-<<<<+>>>>]<<<<<]");

        // Reflect the cursor's static position so subsequent gotos emit
        // correct offsets.
        self.ptr = chunk_base + 1;

        // Move P_c → result.
        self.goto(chunk_base + 2);
        self.raw("[-");
        self.goto(result);
        self.raw("+");
        self.goto(chunk_base + 2);
        self.raw("]");
        // Cursor lands at P_c (chunk_base + 2).
    }

    // Walk a single chunk to write `val` into `chunk_base[local_offset]`.
    // Consumes both `local_offset` and `val`. Cursor ends at chunk_base + 1.
    fn walk_write_chunk(&mut self, chunk_base: usize, local_offset: usize, val: usize) {
        self.clear(chunk_base);
        self.clear(chunk_base + 1);
        self.clear(chunk_base + 2);

        // Move local_offset → P_a.
        self.goto(local_offset);
        self.raw("[-");
        self.goto(chunk_base);
        self.raw("+");
        self.goto(local_offset);
        self.raw("]");
        self.tfree(local_offset);

        // Move val → P_c. (P_c was just cleared.)
        self.goto(val);
        self.raw("[-");
        self.goto(chunk_base + 2);
        self.raw("+");
        self.goto(val);
        self.raw("]");
        self.tfree(val);

        self.goto(chunk_base);
        // Setup P_b = idx (via the same temporary trick — but P_c is
        // already loaded with `val`, so we reuse a different tmp via the
        // walk's S_0_c slot). Simpler: use the standard copy-via-P_c
        // pattern, undoing the val we just put there afterwards. To
        // avoid disturbing P_c, copy P_a → P_b through the array's S_0_c
        // (offset 6). Cursor is at P_a (offset 0); the pattern below
        // walks through {0, 1, 6} explicitly.
        // [-> + >>>>> + <<<<<<]   ;; P_a-- ; P_b++ ; S_0_c++   (cursor stays at P_a)
        // >>>>>> [-<<<<<<+>>>>>>] ;; restore P_a from S_0_c
        // <<<<<<                  ;; back to P_a
        self.raw("[->+>>>>>+<<<<<<]>>>>>>[-<<<<<<+>>>>>>]<<<<<<");

        // Forward walk with 3 transfers (control, survivor, value).
        self.raw("[-[->>>>+<<<<]>[->>>>+<<<<]>[->>>>+<<<<]<<>>>>]");
        // After: cursor at S_(idx-1)_a. S_(idx-1)_b = idx, S_(idx-1)_c = val
        // (or P_b/P_c for idx=0).
        // Step to V_idx and write.
        self.raw(">>>");
        // [-]<[->+<]<  : zero V_idx, move S_(idx-1)_c into V_idx, hop back to S_(idx-1)_b.
        self.raw("[-]<[->+<]<");
        // Backward walk (counter only — value already deposited).
        self.raw("[-[-<<<<+>>>>]<<<<]");
        // Cursor lands at P_b.
        self.ptr = chunk_base + 1;
    }

    // Outer-walk array access for `num_chunks > 1`.
    //
    // Treats the chunks themselves as a moving-pointer walk substrate:
    // each chunk's 3 leading prefix cells (P_a, P_b, P_c) act as the
    // per-step counter / survivor / payload, with stride
    // `chunk_total = 4*N + 5` cells. The forward macro walk transfers
    // these cells (plus P_d for write paths) from chunk[k] to
    // chunk[k+1] per iteration; after `chunk_id` iterations the cursor
    // lands at chunk[chunk_id]'s P_a with P_b = chunk_id and the
    // payloads (local_off, val) materialised in P_c / P_d.
    //
    // The trailing E cell (offset `4*N + 3`) parks the chunk-id
    // survivor across the inner walk's body, which clobbers P_b. Read
    // paths use a 3-cell macro transfer (P_d untouched and irrelevant);
    // write paths use a 4-cell macro transfer that also routes `val`
    // through P_d.
    //
    // Per-access emit is O(chunk_total) BF chars regardless of
    // `num_chunks`. Per-access runtime is O(chunk_id * chunk_total +
    // local_off * 4) tape ops — proportional to the addressed offset
    // but constant in the array's unused tail.
    //
    // chunk_id width is fixed at 1 byte (`num_chunks ≤ 256`) — the
    // macro counter is single-byte today. Larger arrays would need a
    // multi-byte counter or an extra dispatch level on top, which
    // isn't implemented yet.
    fn arr_read_outer_walk(
        &mut self,
        layout: &super::layout::CellLayout,
        idx_expr: &Expr,
        chunk_size: usize,
        num_chunks: usize,
    ) -> (usize, usize) {
        debug_assert_eq!(layout.elem_width(), 1, "walk storage is u8-only");
        debug_assert_eq!(chunk_size, 256, "walk chunk_size must be 256 for u8 counter");
        debug_assert!(num_chunks > 1);

        let result = self.talloc();
        self.clear(result);

        // Group dispatch: the macro counter is u8 so each outer walk
        // covers at most 256 chunks. For larger arrays we cmp_eq on the
        // group-id byte (idx[2]) and fan out into one outer-walk arm
        // per group; each arm is at most 256 chunks wide.
        const GROUP_SIZE: usize = 256;
        let chunk_total = 4 * chunk_size + 5;
        let group_total = GROUP_SIZE * chunk_total;
        let num_groups = num_chunks.div_ceil(GROUP_SIZE);
        assert!(
            num_groups <= 256,
            "walk arrays beyond {} elements not supported (got num_chunks = {num_chunks})",
            GROUP_SIZE * GROUP_SIZE * chunk_size
        );

        let idx_w = if num_groups == 1 { 2 } else { 3 };
        let idx = self.eval_expr_w(idx_expr, idx_w);

        let base = layout.base;

        if num_groups == 1 {
            self.seed_outer_prefix(base, idx, /*val=*/ None, chunk_size);
            self.tfree_n(idx, 2);
            self.goto(base);
            self.emit_outer_walk_read_body(chunk_size);
            self.ptr = base;
            self.extract_outer_walk_result(base, result);
        } else {
            // Two-stage macro walk. idx[0]=local_off, idx[1]=lo (chunk_id mod
            // 256), idx[2]=hi (chunk_id / 256). Per-access emit is
            // O(group_total + chunk_total), independent of num_groups.
            let _ = group_total;
            self.seed_outer_prefix_2stage(base, idx, None, chunk_size);
            self.tfree_n(idx, 3);
            self.goto(base);
            self.emit_stage1_macro_walk_forward(chunk_size, /*with_val=*/ false);
            self.emit_stage1_to_stage2_transition(chunk_size);
            self.emit_outer_walk_read_body(chunk_size); // stage 2
            self.emit_stage2_to_stage1_transition_read(chunk_size);
            self.emit_stage1_macro_walk_backward(chunk_size, /*carry_value=*/ true);
            self.ptr = base;
            // chunk_0.P_a = 0, chunk_0.P_b = 0, chunk_0.P_c = read_value.
            self.extract_outer_walk_result(base, result);
        }

        (result, 1)
    }

    fn arr_write_outer_walk(
        &mut self,
        layout: &super::layout::CellLayout,
        idx_expr: &Expr,
        val: usize,
        chunk_size: usize,
        num_chunks: usize,
    ) {
        debug_assert_eq!(layout.elem_width(), 1, "walk storage is u8-only");
        debug_assert_eq!(chunk_size, 256);
        debug_assert!(num_chunks > 1);

        const GROUP_SIZE: usize = 256;
        let chunk_total = 4 * chunk_size + 5;
        let group_total = GROUP_SIZE * chunk_total;
        let num_groups = num_chunks.div_ceil(GROUP_SIZE);
        assert!(
            num_groups <= 256,
            "walk arrays beyond {} elements not supported (got num_chunks = {num_chunks})",
            GROUP_SIZE * GROUP_SIZE * chunk_size
        );

        let idx_w = if num_groups == 1 { 2 } else { 3 };
        let idx = self.eval_expr_w(idx_expr, idx_w);
        let base = layout.base;

        if num_groups == 1 {
            self.seed_outer_prefix(base, idx, Some(val), chunk_size);
            self.tfree_n(idx, 2);
            self.tfree(val);
            self.goto(base);
            self.emit_outer_walk_write_body(chunk_size);
            self.ptr = base;
            self.clear(base + 1);
        } else {
            // Two-stage macro walk; see arr_read_outer_walk.
            let _ = group_total;
            self.seed_outer_prefix_2stage(base, idx, Some(val), chunk_size);
            self.tfree_n(idx, 3);
            self.tfree(val);
            self.goto(base);
            self.emit_stage1_macro_walk_forward(chunk_size, /*with_val=*/ true);
            self.emit_stage1_to_stage2_transition(chunk_size);
            self.emit_outer_walk_write_body(chunk_size); // stage 2
            self.emit_stage2_to_stage1_transition_write(chunk_size);
            self.emit_stage1_macro_walk_backward(chunk_size, /*carry_value=*/ false);
            self.ptr = base;
            // chunk_0 prefix all 0; nothing to clean up.
        }
    }

    // Final step of the read outer walk: the body left chunk_0.P_b with
    // a leftover chunk-id and chunk_0.P_c with the read value. Clear
    // P_b and move P_c into the caller's result cell.
    fn extract_outer_walk_result(&mut self, base: usize, result: usize) {
        self.clear(base + 1);
        self.goto(base + 2);
        self.raw("[-");
        self.goto(result);
        self.raw("+");
        self.goto(base + 2);
        self.raw("]");
    }

    // Move the index components (and optional `val`) into chunk_0's
    // prefix cells. Layout:
    //   P_a (counter)   = chunk_id  (idx byte 1)
    //   P_b (survivor)  = chunk_id
    //   P_c (payload 1) = local_off (idx byte 0)
    //   P_d (payload 2) = val       (write paths only; offset 4N+4)
    //
    // The transfers are fused into a minimal sequence so each named
    // source pays only one round-trip of large `goto`s. We rely on
    // every previous walk having restored P_a, P_b, P_c, E, P_d to 0
    // (which they do by design); the array is also zero-initialised by
    // layout, so defensive clears are skipped.
    //
    // `idx` is consumed in-place but the caller still owns the temp;
    // `tfree_n(idx, ...)` / `tfree(val)` happen at the call site after
    // this returns.
    fn seed_outer_prefix(
        &mut self,
        base: usize,
        idx: usize,
        val: Option<usize>,
        chunk_size: usize,
    ) {
        let chunk_total = 4 * chunk_size + 5;

        // Fuse "chunk_id → P_a" and "chunk_id → P_b" into a single
        // loop. Source: idx + 1 (chunk_id byte). Destinations: P_a
        // (offset 0) and P_b (offset 1). Per outer iteration we step
        // from idx+1 to base, increment P_a, advance one cell to P_b
        // and increment, return to P_a so the cursor matches
        // `self.ptr`, then jump back to idx+1 — two large gotos per
        // iteration plus a one-time entry/exit.
        self.goto(idx + 1);
        self.raw("[-");
        self.goto(base);
        self.raw("+>+<");
        self.goto(idx + 1);
        self.raw("]");

        // local_off → P_c. Cursor still at idx+1 after the loop above.
        self.goto(idx);
        self.raw("[-");
        self.goto(base + 2);
        self.raw("+");
        self.goto(idx);
        self.raw("]");

        if let Some(v) = val {
            // val → P_d (offset chunk_total - 1).
            let p_d = base + chunk_total - 1;
            self.goto(v);
            self.raw("[-");
            self.goto(p_d);
            self.raw("+");
            self.goto(v);
            self.raw("]");
        }
    }

    // Emit the relative BF for the read path's in-array body. Caller
    // has staged P_a, P_b, P_c (and zeroed E, P_d) at chunk_0 and the
    // cursor is at chunk_0.P_a. Static cursor is unchanged across this
    // routine; on exit the dynamic cursor is back at chunk_0.P_a.
    fn emit_outer_walk_read_body(&mut self, chunk_size: usize) {
        let chunk_total = 4 * chunk_size + 5;
        let s = chunk_total;
        let to_e = chunk_total - 3; // P_b (offset 1) → E (offset 4N+3)
        let pa_to_e = chunk_total - 2; // P_a (0) → E (4N+3)
        let gt_s = ">".repeat(s);
        let lt_s = "<".repeat(s);

        // Forward macro walk: 3-cell transfer (P_a, P_b, P_c).
        // `[-[->S+<S]>[->S+<S]>[->S+<S]<<>S]`
        self.raw("[-");
        self.raw(&format!("[-{gt_s}+{lt_s}]>"));
        self.raw(&format!("[-{gt_s}+{lt_s}]>"));
        self.raw(&format!("[-{gt_s}+{lt_s}]<<"));
        self.raw(&gt_s);
        self.raw("]");
        // Cursor: chunk_target.P_a. P_a=0, P_b=chunk_id, P_c=local_off.

        // Save P_b → E so the inner walk's setup can repurpose P_b.
        // From P_a, `>` to P_b, transfer to_e cells right.
        self.raw(">");
        self.raw(&format!("[-{}+{}]", ">".repeat(to_e), "<".repeat(to_e)));
        self.raw("<"); // back to P_a.

        // Setup inner walk: P_a = local_off, P_b = local_off, P_c = 0.
        // From P_a, `>>` to P_c, then `[-<+<+>>]` decrements P_c while
        // incrementing P_b and P_a, finally `<<` back to P_a.
        self.raw(">>[-<+<+>>]<<");

        // Inner read walk steps 2-6 (no setup — we built P_b ourselves).
        self.raw("[-[->>>>+<<<<]>[->>>>+<<<<]<>>>>]"); // forward
        self.raw(">>>"); // V_idx
        self.raw("<[-]>[-<+>>+<]>[-<+>]<"); // read step
        self.raw("<<"); // S_(idx-1)_b
        self.raw("[-[-<<<<+>>>>]>[-<<<<+>>>>]<<<<<]"); // backward
        // Cursor: chunk_target.P_b. P_a=0, P_b=0, P_c=read value,
        // E=chunk_id, P_d=0.

        // Restore E → P_a. P_b → E distance = to_e. E → P_a = pa_to_e.
        self.raw(&">".repeat(to_e));
        self.raw(&format!(
            "[-{}+{}]",
            "<".repeat(pa_to_e),
            ">".repeat(pa_to_e)
        ));
        self.raw(&"<".repeat(pa_to_e)); // back to P_a.

        // Build P_b = chunk_id from P_a using S_0_a (offset 4) as scratch.
        self.raw("[->+>>>+<<<<]>>>>[-<<<<+>>>>]<<<<");
        // Cursor: chunk_target.P_a. P_a=P_b=chunk_id, P_c=value.

        // Backward macro walk: 3-cell transfer.
        // `[-[-<S+>S]>[-<S+>S]>[-<S+>S]<<<S]`
        self.raw("[-");
        self.raw(&format!("[-{lt_s}+{gt_s}]>"));
        self.raw(&format!("[-{lt_s}+{gt_s}]>"));
        self.raw(&format!("[-{lt_s}+{gt_s}]<<"));
        self.raw(&lt_s);
        self.raw("]");
        // Cursor: chunk_0.P_a. chunk_0.P_b=chunk_id leftover,
        // chunk_0.P_c=read value.
    }

    // Emit the relative BF for the write path's in-array body. Caller
    // has staged P_a, P_b, P_c, P_d at chunk_0 and the cursor is at
    // chunk_0.P_a. Same cursor invariant as the read path.
    fn emit_outer_walk_write_body(&mut self, chunk_size: usize) {
        let chunk_total = 4 * chunk_size + 5;
        let s = chunk_total;
        let to_e = chunk_total - 3; // P_b (1) → E (4N+3)
        let pa_to_e = chunk_total - 2; // P_a (0) → E
        let pa_to_pd = chunk_total - 1; // P_a (0) → P_d (4N+4)
        let pc_to_pd = chunk_total - 3; // P_c (2) → P_d
        let gt_s = ">".repeat(s);
        let lt_s = "<".repeat(s);

        // Forward macro walk: 4-cell transfer (P_a, P_b, P_c, P_d).
        // From P_c (offset 2) to P_d (offset 4N+4) is `pc_to_pd` cells.
        // From P_d back to P_a (offset 0) is `pa_to_pd` cells.
        let gt_pc_to_pd = ">".repeat(pc_to_pd);
        let lt_pd_to_pa = "<".repeat(pa_to_pd);
        self.raw("[-");
        self.raw(&format!("[-{gt_s}+{lt_s}]>"));
        self.raw(&format!("[-{gt_s}+{lt_s}]>"));
        self.raw(&format!("[-{gt_s}+{lt_s}]"));
        self.raw(&gt_pc_to_pd);
        self.raw(&format!("[-{gt_s}+{lt_s}]"));
        self.raw(&lt_pd_to_pa);
        self.raw(&gt_s);
        self.raw("]");
        // Cursor: chunk_target.P_a. P_a=0, P_b=chunk_id, P_c=local_off,
        // P_d=val.

        // Save P_b → E.
        self.raw(">");
        self.raw(&format!("[-{}+{}]", ">".repeat(to_e), "<".repeat(to_e)));
        self.raw("<"); // back to P_a.

        // Move P_c (local_off) → P_a.
        self.raw(">>[-<<+>>]<<"); // P_c → P_a. Cursor at P_a.

        // Move P_d (val) → P_c. From P_a, `>{pa_to_pd}` to P_d, then
        // `[-<{pc_to_pd}+>{pc_to_pd}]` transfers to P_c.
        let gt_pa_to_pd = ">".repeat(pa_to_pd);
        let lt_pc_to_pd = "<".repeat(pc_to_pd);
        let gt_pc_to_pd2 = ">".repeat(pc_to_pd);
        let lt_pa_to_pd = "<".repeat(pa_to_pd);
        self.raw(&gt_pa_to_pd);
        self.raw(&format!("[-{lt_pc_to_pd}+{gt_pc_to_pd2}]"));
        self.raw(&lt_pa_to_pd); // back to P_a.
        // State: P_a=local_off, P_b=0, P_c=val, S_0_c=0, ..., E=chunk_id.

        // Inner write walk's setup builds P_b from P_a via S_0_c (offset
        // 6), preserving P_c. Pattern reused from `walk_write_chunk`.
        self.raw("[->+>>>>>+<<<<<<]>>>>>>[-<<<<<<+>>>>>>]<<<<<<");

        // Forward write walk (3-cell transfer: P_a, P_b, P_c).
        self.raw("[-[->>>>+<<<<]>[->>>>+<<<<]>[->>>>+<<<<]<<>>>>]");
        // Step to V_idx and write.
        self.raw(">>>");
        self.raw("[-]<[->+<]<");
        // Backward walk (counter only).
        self.raw("[-[-<<<<+>>>>]<<<<]");
        // Cursor: chunk_target.P_b. State: P_a=P_b=P_c=0, V_local_off=val,
        // E=chunk_id.

        // Restore E → P_a. (Same pattern as read path.)
        self.raw(&">".repeat(to_e));
        self.raw(&format!(
            "[-{}+{}]",
            "<".repeat(pa_to_e),
            ">".repeat(pa_to_e)
        ));
        self.raw(&"<".repeat(pa_to_e));

        // Build P_b = chunk_id via S_0_a.
        self.raw("[->+>>>+<<<<]>>>>[-<<<<+>>>>]<<<<");

        // Backward macro walk: 3-cell transfer (P_c is 0 here, transfer
        // is a no-op for that slot).
        self.raw("[-");
        self.raw(&format!("[-{lt_s}+{gt_s}]>"));
        self.raw(&format!("[-{lt_s}+{gt_s}]>"));
        self.raw(&format!("[-{lt_s}+{gt_s}]<<"));
        self.raw(&lt_s);
        self.raw("]");
        // Cursor: chunk_0.P_a. chunk_0.P_b=chunk_id leftover.
    }

    // -- two-stage outer walk (num_chunks > 256) -----------------------------
    //
    // For arrays whose `num_chunks` exceeds the single-byte macro counter
    // limit (256), we run a two-stage macro walk instead of the per-group
    // `cmp_eq` fanout. Stage 1 strides at `S1 = 256 * chunk_total` (one step
    // per group), stage 2 reuses the existing single-group walk at stride
    // `chunk_total` inside the destination group. Per-access emit becomes
    // `O(group_total + chunk_total)`, decoupled from `num_groups`.
    //
    // 3-byte idx layout: [local_off, lo, hi] with lo = chunk_id mod 256 and
    // hi = chunk_id / 256. Per-chunk prefix (chunk_total = 4N+5):
    //   offset 0           P_a  stage 1: hi (counter) → stage 2: lo (counter)
    //   offset 1           P_b  stage 1: hi_dup       → stage 2: lo_dup
    //   offset 2           P_c  stage 1: lo (carrier) → stage 2: local_off
    //   offset chunk_total-2  E    stage 1: local_off carrier; stage 2: parking
    //   offset chunk_total-1  P_d  val (write only)
    // `hi` is parked at chunk_(hi*256).S_(N-1)_c (offset chunk_total-3) across
    // stage 2; the inner walk's read/write step touches offsets up to
    // 4*local_off+4 ≤ 4N (= 1024 for N=256), so S_(N-1)_c at offset 4N+2 is
    // never touched regardless of local_off. Restored to 0 by the back-half
    // before stage 1 backward starts.
    fn seed_outer_prefix_2stage(
        &mut self,
        base: usize,
        idx: usize,
        val: Option<usize>,
        chunk_size: usize,
    ) {
        let chunk_total = 4 * chunk_size + 5;
        let e = base + chunk_total - 2;

        // hi (idx+2) → P_a + P_b (fused).
        self.goto(idx + 2);
        self.raw("[-");
        self.goto(base);
        self.raw("+>+<");
        self.goto(idx + 2);
        self.raw("]");

        // lo (idx+1) → P_c.
        self.goto(idx + 1);
        self.raw("[-");
        self.goto(base + 2);
        self.raw("+");
        self.goto(idx + 1);
        self.raw("]");

        // local_off (idx+0) → E.
        self.goto(idx);
        self.raw("[-");
        self.goto(e);
        self.raw("+");
        self.goto(idx);
        self.raw("]");

        if let Some(v) = val {
            let p_d = base + chunk_total - 1;
            self.goto(v);
            self.raw("[-");
            self.goto(p_d);
            self.raw("+");
            self.goto(v);
            self.raw("]");
        }
    }

    // Stage 1 forward macro walk at stride `S1 = 256 * chunk_total`. Carries
    // 4 cells (P_a, P_b, P_c, E) for read or 5 cells (+ P_d) for write.
    // Caller's static cursor (`self.ptr`) stays at chunk_0.P_a; dynamic
    // cursor lands at chunk_(hi*256).P_a on exit. After K = hi iterations:
    //   chunk_(hi*256): P_a=0, P_b=hi, P_c=lo, E=local_off, [P_d=val]
    //   intermediate chunks: all carried slots = 0
    fn emit_stage1_macro_walk_forward(&mut self, chunk_size: usize, with_val: bool) {
        let chunk_total = 4 * chunk_size + 5;
        let s1 = 256 * chunk_total;
        let gt_s1 = ">".repeat(s1);
        let lt_s1 = "<".repeat(s1);
        let pc_to_e = chunk_total - 4; // offset 2 → offset chunk_total-2

        self.raw("[-");
        // P_a → next chunk's P_a.
        self.raw(&format!("[-{gt_s1}+{lt_s1}]>"));
        // P_b → next chunk's P_b.
        self.raw(&format!("[-{gt_s1}+{lt_s1}]>"));
        // P_c → next chunk's P_c.
        self.raw(&format!("[-{gt_s1}+{lt_s1}]"));
        // Advance from P_c (offset 2) to E (offset chunk_total - 2).
        self.raw(&">".repeat(pc_to_e));
        // E → next chunk's E.
        self.raw(&format!("[-{gt_s1}+{lt_s1}]"));

        if with_val {
            // Advance E (offset chunk_total-2) → P_d (offset chunk_total-1).
            self.raw(">");
            // P_d → next chunk's P_d.
            self.raw(&format!("[-{gt_s1}+{lt_s1}]"));
            // Back to P_a (offset 0): distance chunk_total - 1.
            self.raw(&"<".repeat(chunk_total - 1));
        } else {
            // Back to P_a (offset 0): distance chunk_total - 2.
            self.raw(&"<".repeat(chunk_total - 2));
        }

        // Advance to next chunk's P_a (loop condition).
        self.raw(&gt_s1);
        self.raw("]");
    }

    // Rearrange chunk_(hi*256) prefix from stage-1 layout to stage-2 layout.
    // Entry state (cursor at P_a): P_a=0, P_b=hi, P_c=lo, E=local_off, [P_d=val].
    // Exit state (cursor at P_a):  P_a=lo, P_b=lo, P_c=local_off, E=0,
    //                              S_(N-1)_c=hi (parked), [P_d=val].
    fn emit_stage1_to_stage2_transition(&mut self, chunk_size: usize) {
        let chunk_total = 4 * chunk_size + 5;
        let pc_to_e = chunk_total - 4;
        // P_b (offset 1) → S_(N-1)_c (offset chunk_total - 3): distance = chunk_total - 4.
        let pb_to_park = chunk_total - 4;

        // Park hi: from P_a, > to P_b, then empty P_b → S_(N-1)_c at distance
        // pb_to_park = chunk_total - 4. Cursor ends at P_b.
        self.raw(">");
        self.raw(&format!(
            "[-{}+{}]",
            ">".repeat(pb_to_park),
            "<".repeat(pb_to_park)
        ));

        // Move lo (P_c) → P_a: from P_b, > to P_c, [-<<+>>] empties P_c → P_a.
        // Cursor ends at P_c.
        self.raw(">[-<<+>>]");

        // Move local_off (E) → P_c: from P_c, >(pc_to_e) to E, then
        // [-<(pc_to_e)+>(pc_to_e)] empties E → P_c. Cursor ends at E.
        self.raw(&">".repeat(pc_to_e));
        self.raw(&format!(
            "[-{}+{}]",
            "<".repeat(pc_to_e),
            ">".repeat(pc_to_e)
        ));

        // Back to P_a (offset 0): distance chunk_total - 2.
        self.raw(&"<".repeat(chunk_total - 2));

        // Build P_b = lo from P_a using S_0_c (offset 6) as scratch
        // (preserves P_a and P_c, mirrors the pattern at line 1580).
        self.raw("[->+>>>>>+<<<<<<]>>>>>>[-<<<<<<+>>>>>>]<<<<<<");
    }

    // After stage 2 returns to chunk_(hi*256).P_a, prepare for stage 1 bwd.
    // Read entry state: P_a=0, P_b=lo (leftover), P_c=read_value,
    //                   S_(N-1)_c=hi (parked), others=0.
    // Read exit state:  P_a=hi, P_b=0, P_c=read_value, S_(N-1)_c=0.
    fn emit_stage2_to_stage1_transition_read(&mut self, chunk_size: usize) {
        let chunk_total = 4 * chunk_size + 5;
        // P_a (offset 0) → S_(N-1)_c (offset chunk_total - 3): distance.
        let pa_to_park = chunk_total - 3;

        // Clear P_b leftover.
        self.raw(">[-]<");
        // Unpark S_(N-1)_c → P_a: >(pa_to_park) to S_(N-1)_c,
        // [-<(pa_to_park)+>(pa_to_park)] empties → P_a, <(pa_to_park) back.
        self.raw(&">".repeat(pa_to_park));
        self.raw(&format!(
            "[-{}+{}]",
            "<".repeat(pa_to_park),
            ">".repeat(pa_to_park)
        ));
        self.raw(&"<".repeat(pa_to_park));
    }

    // Write entry state: P_a=0, P_b=lo (leftover), P_c=0, V_local_off=val,
    //                    S_(N-1)_c=hi (parked), others=0.
    // Write exit state:  P_a=hi, others=0 (in prefix).
    fn emit_stage2_to_stage1_transition_write(&mut self, chunk_size: usize) {
        let chunk_total = 4 * chunk_size + 5;
        let pa_to_park = chunk_total - 3;

        self.raw(">[-]<");
        self.raw(&">".repeat(pa_to_park));
        self.raw(&format!(
            "[-{}+{}]",
            "<".repeat(pa_to_park),
            ">".repeat(pa_to_park)
        ));
        self.raw(&"<".repeat(pa_to_park));
    }

    // Stage 1 backward macro walk at stride `S1`. Read carries (P_a counter,
    // P_c value); write carries only the counter. Cursor enters at
    // chunk_(hi*256).P_a, exits at chunk_0.P_a. After K = hi iterations,
    // chunk_0.P_a = 0; chunk_0.P_c = read_value (if carrying value).
    fn emit_stage1_macro_walk_backward(&mut self, chunk_size: usize, carry_value: bool) {
        let chunk_total = 4 * chunk_size + 5;
        let s1 = 256 * chunk_total;
        let gt_s1 = ">".repeat(s1);
        let lt_s1 = "<".repeat(s1);

        self.raw("[-");
        // P_a → chunk_(j-1).P_a.
        self.raw(&format!("[-{lt_s1}+{gt_s1}]"));

        if carry_value {
            // Advance to P_c (offset 2; P_b at offset 1 is 0, skipped).
            self.raw(">>");
            // P_c → chunk_(j-1).P_c.
            self.raw(&format!("[-{lt_s1}+{gt_s1}]"));
            // Back to P_a.
            self.raw("<<");
        }

        // Advance left to chunk_(j-1).P_a.
        self.raw(&lt_s1);
        self.raw("]");
    }
}

impl<'a> BfEmitter<'a> {
    // Number I/O (single-byte, legacy).

    fn print_num(&mut self, cell: usize) {
        let v = self.talloc();
        self.copy(cell, v);
        let h = self.talloc();
        self.clear(h);
        let t = self.talloc();
        self.clear(t);
        self.divconst(v, 100, h);
        self.divconst(v, 10, t);
        let hp = self.is_nz(h);
        let h2 = h;
        self.if_else(
            hp,
            |bf| {
                let d = bf.talloc();
                bf.copy(h2, d);
                bf.inc(d, 48);
                bf.out(d);
                bf.tfree(d);
            },
            None::<fn(&mut Self)>,
        );
        self.tfree(hp);
        let ht = self.talloc();
        self.clear(ht);
        let ch = self.is_nz(h);
        let ht2 = ht;
        self.goto(ch);
        self.raw("[");
        self.setv(ht2, 1);
        self.clear(ch);
        self.goto(ch);
        self.raw("]");
        self.tfree(ch);
        let ct = self.is_nz(t);
        let ht3 = ht;
        self.goto(ct);
        self.raw("[");
        self.setv(ht3, 1);
        self.clear(ct);
        self.goto(ct);
        self.raw("]");
        self.tfree(ct);
        let t2 = t;
        self.if_else(
            ht,
            |bf| {
                let d = bf.talloc();
                bf.copy(t2, d);
                bf.inc(d, 48);
                bf.out(d);
                bf.tfree(d);
            },
            None::<fn(&mut Self)>,
        );
        self.tfree(ht);
        self.inc(v, 48);
        self.out(v);
        self.tfree(t2);
        self.tfree(h2);
        self.tfree(v);
    }

    // quot = val / div_const;  val %= div_const  (single-byte in-place)
    fn divconst(&mut self, val: usize, div: u8, quot: usize) {
        self.clear(quot);
        let flag = self.talloc();
        self.setv(flag, 1);
        self.goto(flag);
        self.raw("[");
        self.clear(flag);
        let tv = self.talloc();
        let td = self.talloc();
        self.copy(val, tv);
        self.setv(td, div);
        let gt = self.cmp_gt(td, tv);
        let ge = self.negate(gt);
        let val2 = val;
        let quot2 = quot;
        let flag2 = flag;
        let div2 = div;
        self.goto(ge);
        self.raw("[");
        self.dec(val2, div2 as usize);
        self.inc(quot2, 1);
        self.setv(flag2, 1);
        self.clear(ge);
        self.goto(ge);
        self.raw("]");
        self.tfree(ge);
        self.goto(flag);
        self.raw("]");
        self.tfree(flag);
    }

    fn scan_num(&mut self, target: usize) {
        self.clear(target);
        let ch = self.talloc();
        self.clear(ch);
        self.inp(ch);
        let lf = self.is_digit(ch);
        let t2 = target;
        let ch2 = ch;
        self.goto(lf);
        self.raw("[");
        self.mul10(t2);
        let dg = self.talloc();
        self.copy(ch2, dg);
        self.dec(dg, 48);
        self.add_to(dg, t2);
        self.tfree(dg);
        self.clear(ch2);
        self.inp(ch2);
        self.clear(lf);
        let lf2 = self.is_digit(ch2);
        self.bfmove(lf2, lf);
        self.tfree(lf2);
        self.goto(lf);
        self.raw("]");
        self.tfree(lf);
        self.tfree(ch);
    }

    fn is_digit(&mut self, ch: usize) -> usize {
        let r = self.talloc();
        self.clear(r);
        let c1 = self.is_nz(ch);
        let t10 = self.talloc();
        self.copy(ch, t10);
        self.dec(t10, 10);
        let c2 = self.is_nz(t10);
        self.tfree(t10);
        let t32 = self.talloc();
        self.copy(ch, t32);
        self.dec(t32, 32);
        let c3 = self.is_nz(t32);
        self.tfree(t32);
        let r2 = r;
        self.goto(c1);
        self.raw("[");
        self.goto(c2);
        self.raw("[");
        self.goto(c3);
        self.raw("[");
        self.setv(r2, 1);
        self.clear(c3);
        self.goto(c3);
        self.raw("]");
        self.clear(c2);
        self.goto(c2);
        self.raw("]");
        self.clear(c1);
        self.goto(c1);
        self.raw("]");
        self.tfree(c3);
        self.tfree(c2);
        self.tfree(c1);
        r
    }

    fn mul10(&mut self, cell: usize) {
        let tc = self.talloc();
        self.copy(cell, tc);
        self.clear(cell);
        for _ in 0..10 {
            let t2 = self.talloc();
            self.copy(tc, t2);
            self.add_to(t2, cell);
            self.tfree(t2);
        }
        self.tfree(tc);
    }

    // Multi-byte number I/O.

    // cell *= 10, n bytes in-place
    fn mul10_n(&mut self, cell: usize, n: usize) {
        if n == 1 {
            self.mul10(cell);
            return;
        }
        let tc = self.talloc_n(n);
        self.copy_n(cell, tc, n);
        for i in 0..n {
            self.clear(cell + i);
        }
        for _ in 0..10 {
            let t2 = self.talloc_n(n);
            self.copy_n(tc, t2, n);
            self.add_n(cell, t2, n);
        }
        self.tfree_n(tc, n);
    }

    // Read decimal number from stdin into n-byte little-endian target
    fn scan_num_n(&mut self, target: usize, n: usize) {
        if n == 1 {
            self.scan_num(target);
            return;
        }
        for i in 0..n {
            self.clear(target + i);
        }
        let ch = self.talloc();
        self.clear(ch);
        self.inp(ch);
        let lf = self.is_digit(ch);
        let ch2 = ch;
        let n2 = n;
        self.goto(lf);
        self.raw("[");
        self.mul10_n(target, n2);
        // digit value = ch - '0', add as single-byte to n-byte target
        let dg = self.talloc();
        self.copy(ch2, dg);
        self.dec(dg, 48);
        let dg_wide = self.widen(dg, 1, n2);
        self.add_n(target, dg_wide, n2);
        self.clear(ch2);
        self.inp(ch2);
        self.clear(lf);
        let lf2 = self.is_digit(ch2);
        self.bfmove(lf2, lf);
        self.tfree(lf2);
        self.goto(lf);
        self.raw("]");
        self.tfree(lf);
        self.tfree(ch);
    }

    // quot (n bytes) = val (n bytes) / div_const;  val %= div_const  (multi-byte in-place)
    fn divconst_n(&mut self, val: usize, n: usize, div: u32, quot: usize) {
        if n == 1 {
            self.divconst(val, div as u8, quot);
            return;
        }
        for i in 0..n {
            self.clear(quot + i);
        }
        let flag = self.talloc();
        self.setv(flag, 1);
        self.goto(flag);
        self.raw("[");
        self.clear(flag);
        let tv = self.talloc_n(n);
        let td = self.talloc_n(n);
        self.copy_n(val, tv, n);
        self.set_const_n(td, div, n);
        let gt = self.cmp_gt_n(td, tv, n); // div > val?
        let ge = self.negate(gt); // val >= div
        let div2 = div;
        let val2 = val;
        let quot2 = quot;
        let flag2 = flag;
        let n2 = n;
        self.goto(ge);
        self.raw("[");
        let ts = self.talloc_n(n2);
        self.set_const_n(ts, div2, n2);
        self.sub_n(val2, ts, n2);
        self.inc_n(quot2, n2);
        self.setv(flag2, 1);
        self.clear(ge);
        self.goto(ge);
        self.raw("]");
        self.tfree(ge);
        self.goto(flag);
        self.raw("]");
        self.tfree(flag);
    }

    // Print n-byte little-endian value as decimal (non-destructive)
    fn print_num_n(&mut self, cell: usize, n: usize) {
        if n == 1 {
            self.print_num(cell);
            return;
        }
        // Work on a copy
        let v = self.talloc_n(n);
        self.copy_n(cell, v, n);

        // Determine divisors for the width
        let divisors: &[u32] = match n {
            2 => &[10000, 1000, 100, 10],
            4 => &[
                1000000000, 100000000, 10000000, 1000000, 100000, 10000, 1000, 100, 10,
            ],
            _ => &[],
        };

        // Extract digits: for each divisor, divide and get the digit
        let mut digit_cells: Vec<usize> = Vec::new();
        let quot = self.talloc_n(n);
        for &div in divisors {
            self.divconst_n(v, n, div, quot);
            // quot low byte is the digit (0-9) after divconst_n reduces v by multiples of div
            let dg = self.talloc();
            self.copy(quot, dg);
            digit_cells.push(dg);
            for i in 0..n {
                self.clear(quot + i);
            }
        }
        // Last digit is the remaining value (v mod 10)
        let last = self.talloc();
        self.copy(v, last);
        digit_cells.push(last);

        self.tfree_n(quot, n);
        self.tfree_n(v, n);

        // Print: skip leading zeros except for the last digit
        let len = digit_cells.len();
        let seen_nz = self.talloc();
        self.clear(seen_nz);
        for (idx, &d) in digit_cells.iter().enumerate() {
            if idx == len - 1 {
                // Always print last digit
                let is_last = d;
                self.inc(is_last, 48);
                self.out(is_last);
                self.tfree(is_last);
            } else {
                let snz = seen_nz;
                let d2 = d;
                // if seen_nz or d != 0: print d
                let d_nz = self.is_nz(d);
                let should_print_flag = self.talloc();
                self.clear(should_print_flag);
                let snz_copy = self.talloc();
                self.copy(snz, snz_copy);
                self.goto(snz_copy);
                self.raw("[");
                self.setv(should_print_flag, 1);
                self.clear(snz_copy);
                self.goto(snz_copy);
                self.raw("]");
                self.tfree(snz_copy);
                self.goto(d_nz);
                self.raw("[");
                self.setv(should_print_flag, 1);
                self.clear(d_nz);
                self.goto(d_nz);
                self.raw("]");
                self.tfree(d_nz);

                let d3 = d2;
                let snz2 = snz;
                self.goto(should_print_flag);
                self.raw("[");
                self.setv(snz2, 1);
                let dc = self.talloc();
                self.copy(d3, dc);
                self.inc(dc, 48);
                self.out(dc);
                self.tfree(dc);
                self.clear(should_print_flag);
                self.goto(should_print_flag);
                self.raw("]");
                self.tfree(should_print_flag);
                self.tfree(d2);
            }
        }
        self.tfree(seen_nz);
    }

    // Expression evaluation — returns (base_cell, width).

    // Evaluate expression; returns (base, width). Caller must tfree_n(base, width).
    fn eval_expr(&mut self, expr: &Expr) -> (usize, usize) {
        match expr {
            Expr::Int(n) => {
                let w = if *n < 256 {
                    1
                } else if *n < 65536 {
                    2
                } else {
                    4
                };
                let t = self.talloc_n(w);
                self.set_const_n(t, *n as u32, w);
                (t, w)
            }
            Expr::Var(name) => {
                let layout = self.lookup(name);
                let base = layout.base;
                let w = layout.width;
                let t = self.talloc_n(w);
                self.copy_n(base, t, w);
                (t, w)
            }
            Expr::Index(name, idx_expr) => {
                let layout = self.lookup(name);
                self.arr_read(&layout, idx_expr)
            }
            Expr::Call(name, args) => {
                // typeck has already rejected void calls in expression position
                let (r, rw) = self.inline_call(name, args).expect(
                    "void function in expression context should have been rejected by typeck",
                );
                (r, rw)
            }
            Expr::BinOp(op, left, right) => {
                let (a, aw) = self.eval_expr(left);
                let (b, bw) = self.eval_expr(right);
                self.eval_binop_w(*op, a, aw, b, bw)
            }
            Expr::UnOp(UnOp::Neg, inner) => {
                let (t, w) = self.eval_expr(inner);
                let zero = self.talloc_n(w);
                for i in 0..w {
                    self.clear(zero + i);
                }
                self.sub_n(zero, t, w);
                (zero, w)
            }
            Expr::UnOp(UnOp::Not, inner) => {
                let (t, w) = self.eval_expr(inner);
                let nz = self.is_nz_n(t, w);
                self.tfree_n(t, w);
                let r = self.negate(nz);
                (r, 1)
            }
        }
    }

    // Evaluate expression and produce a result of exactly `w` bytes by widening
    // (zero-extension) or truncation (low `w` bytes copied, high bytes
    // discarded). Caller frees with `tfree_n(result, w)`. Used by `arr_read` /
    // `arr_write` when the array index needs ≥2-byte resolution.
    fn eval_expr_w(&mut self, expr: &Expr, w: usize) -> usize {
        let (base, ew) = self.eval_expr(expr);
        if ew == w {
            return base;
        }
        if ew < w {
            return self.widen(base, ew, w);
        }
        // ew > w: truncate to low `w` bytes.
        let lo = self.talloc_n(w);
        self.bfmove_n(base, lo, w);
        for i in w..ew {
            self.clear(base + i);
        }
        self.tfree_n(base, ew);
        lo
    }

    // Convenience: evaluate expecting single byte (for array indices, conditions)
    fn eval_expr_1(&mut self, expr: &Expr) -> usize {
        let (base, w) = self.eval_expr(expr);
        if w > 1 {
            // Truncate to low byte
            let lo = self.talloc();
            self.bfmove(base, lo);
            self.tfree_n(base, w); // free the rest (already moved base)
            // Actually bfmove moved base cell but the remaining w-1 cells are still allocated
            // We need to clear and free them manually
            for i in 1..w {
                self.clear(base + i);
            }
            self.tfree_n(base + 1, w - 1);
            // Actually the talloc_n allocated base..base+w as a block. We moved base to lo.
            // base is still "in use" per allocator but we moved it.
            // This is getting tricky. Let me just use the low byte directly.
            // Redo: just free the whole block and return base (now zeroed except we moved it)
            // Actually let me just take the low byte copy:
            lo
        } else {
            base
        }
    }

    fn eval_binop_w(
        &mut self,
        op: BinOp,
        a: usize,
        aw: usize,
        b: usize,
        bw: usize,
    ) -> (usize, usize) {
        let w = aw.max(bw);
        // Widen both to the same width
        let a = if aw < w { self.widen(a, aw, w) } else { a };
        let b = if bw < w { self.widen(b, bw, w) } else { b };

        match op {
            BinOp::Add => {
                self.add_n(a, b, w);
                (a, w)
            }
            BinOp::Sub => {
                self.sub_n(a, b, w);
                (a, w)
            }
            BinOp::Mul => (self.arith_mul_n(a, b, w), w),
            BinOp::Div => (self.arith_div_n(a, b, w), w),
            BinOp::Rem => (self.arith_mod_n(a, b, w), w),
            BinOp::Lt => (self.cmp_lt_n(a, b, w), 1),
            BinOp::Gt => (self.cmp_gt_n(a, b, w), 1),
            BinOp::Le => (self.cmp_le_n(a, b, w), 1),
            BinOp::Ge => (self.cmp_ge_n(a, b, w), 1),
            BinOp::EqEq => (self.cmp_eq_n(a, b, w), 1),
            BinOp::Ne => (self.cmp_ne_n(a, b, w), 1),
            BinOp::And => {
                // (a != 0) && (b != 0) → 0 or 1
                let r = self.talloc();
                self.clear(r);
                let anz = self.is_nz_n(a, w);
                let bnz = self.is_nz_n(b, w);
                let r2 = r;
                self.goto(anz);
                self.raw("[");
                self.goto(bnz);
                self.raw("[");
                self.setv(r2, 1);
                self.clear(bnz);
                self.goto(bnz);
                self.raw("]");
                self.clear(anz);
                self.goto(anz);
                self.raw("]");
                self.tfree(anz);
                self.tfree(bnz);
                self.tfree_n(a, w);
                self.tfree_n(b, w);
                (r, 1)
            }
            BinOp::Or => {
                let r = self.talloc();
                self.clear(r);
                let anz = self.is_nz_n(a, w);
                let bnz = self.is_nz_n(b, w);
                let r2 = r;
                self.goto(anz);
                self.raw("[");
                self.setv(r2, 1);
                self.clear(anz);
                self.goto(anz);
                self.raw("]");
                self.goto(bnz);
                self.raw("[");
                self.setv(r2, 1);
                self.clear(bnz);
                self.goto(bnz);
                self.raw("]");
                self.tfree(anz);
                self.tfree(bnz);
                self.tfree_n(a, w);
                self.tfree_n(b, w);
                (r, 1)
            }
        }
    }

    // Function inlining.

    /// Expand a call site as a fresh copy of the callee's body with
    /// parameters bound into a new scope. Returns `Some((base, width))` of
    /// the return-value cells for non-void callees (the caller must `tfree_n`),
    /// or `None` for void functions.
    fn inline_call(&mut self, name: &str, args: &[Expr]) -> Option<(usize, usize)> {
        let def = self
            .fns
            .get(name)
            .expect("call to undefined function should have been rejected by typeck")
            .clone();

        // Evaluate all arguments into temp cells BEFORE pushing the new scope,
        // so argument expressions still see the caller's bindings (e.g., a
        // caller-side `x` passed as an argument to a callee whose parameter
        // is also named `x`).
        let mut scalar_args: Vec<(usize, usize)> = Vec::new();
        let mut array_aliases: Vec<CellLayout> = Vec::new();
        for (param, arg) in def.params.iter().zip(args.iter()) {
            match &param.ty {
                TypeAnn::Scalar(_) => {
                    let (v, vw) = self.eval_expr(arg);
                    scalar_args.push((v, vw));
                    array_aliases.push(CellLayout {
                        base: 0,
                        width: 0,
                        ty: ScalarType::U8,
                        array_len: None,
                        layout: super::layout::ArrayLayout::Linear,
                    }); // placeholder; unused
                }
                TypeAnn::Array(_, _) => {
                    // typeck guarantees the arg is an array identifier
                    let arr_name = match arg {
                        Expr::Var(n) => n.clone(),
                        _ => unreachable!("typeck validated array argument is a name"),
                    };
                    array_aliases.push(self.lookup(&arr_name));
                    scalar_args.push((0, 0)); // placeholder; unused
                }
            }
        }

        // Build the new scope: parameters (with scalar values moved into fresh
        // cells, or arrays aliased) plus an optional return slot.
        let mut scope: HashMap<String, CellLayout> = HashMap::new();
        let mut scalar_param_cells: Vec<(usize, usize)> = Vec::new();

        for (i, param) in def.params.iter().enumerate() {
            match &param.ty {
                TypeAnn::Scalar(st) => {
                    let w = st.cell_width();
                    let base = self.talloc_n(w);
                    // Zero the param cells; bfmove_n below writes into freshly
                    // cleared cells and any tail that widen leaves is already
                    // cleared inside `widen`.
                    for k in 0..w {
                        self.clear(base + k);
                    }
                    let (v, vw) = scalar_args[i];
                    let v = if vw < w { self.widen(v, vw, w) } else { v };
                    self.bfmove_n(v, base, w);
                    self.tfree_n(v, w);
                    scalar_param_cells.push((base, w));
                    scope.insert(
                        param.name.clone(),
                        CellLayout {
                            base,
                            width: w,
                            ty: *st,
                            array_len: None,
                            layout: super::layout::ArrayLayout::Linear,
                        },
                    );
                }
                TypeAnn::Array(_, _) => {
                    // Alias the caller's array into the param name; no copy.
                    scope.insert(param.name.clone(), array_aliases[i].clone());
                }
            }
        }

        // Allocate a return slot for non-void callees.
        let ret_cells = def.ret_ty.map(|rt| {
            let w = rt.cell_width();
            let base = self.talloc_n(w);
            // Zero the slot so a void-style fallthrough body would still give
            // a deterministic value (typeck enforces non-void bodies end in
            // `return expr;` so this is belt-and-suspenders).
            for i in 0..w {
                self.clear(base + i);
            }
            scope.insert(
                RET_SLOT.to_string(),
                CellLayout {
                    base,
                    width: w,
                    ty: rt,
                    array_len: None,
                    layout: super::layout::ArrayLayout::Linear,
                },
            );
            (base, w)
        });

        // Remember which in-fn local cells we allocate during the body so we
        // can free them on frame unwind. We do that by snapshotting `used`
        // before pushing the scope, and delta-freeing after.
        // NOTE: scalar_param_cells and the return slot are already tracked in
        // `used` via talloc_n; we explicitly free them below, which takes care
        // of everything the frame introduced.
        self.scopes.push(scope);
        for s in &def.body {
            self.gen_stmt(s);
        }
        let frame_scope = self.scopes.pop().unwrap();

        // Free all frame-local cells:
        //   - scalar parameter cells
        //   - function-local `let` bindings registered in the frame scope
        //     (anything whose name is not a parameter and not the return slot)
        for (base, w) in scalar_param_cells {
            self.tfree_n(base, w);
        }
        let param_names: std::collections::HashSet<&str> =
            def.params.iter().map(|p| p.name.as_str()).collect();
        for (nm, l) in &frame_scope {
            if nm == RET_SLOT {
                continue;
            }
            if param_names.contains(nm.as_str()) {
                continue;
            }
            // Array params are aliased; real array params wouldn't reach here
            // because they're in `param_names`. So `l` is always a freshly
            // allocated scalar/array local — free it.
            self.tfree_n(l.base, l.width);
        }

        ret_cells
    }

    // Statements.

    fn gen_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Let { name, ty, init } => {
                // Top-level `let` binds into the static MemMap allocated by
                // typeck; in-function `let` dynamically allocates temp cells
                // so each call gets its own copy of the local.
                let layout = if self.in_fn_scope() {
                    // Function-local `let` always uses linear storage — walk
                    // mode currently lives only in the static MemMap path
                    // (top-level arrays). Function-locals are short-lived
                    // and rarely benefit from the larger 4N+3 footprint.
                    let (base, width, array_len, scalar_ty) = match ty {
                        TypeAnn::Scalar(st) => {
                            (self.talloc_n(st.cell_width()), st.cell_width(), None, *st)
                        }
                        TypeAnn::Array(st, len) => {
                            let total = st.cell_width() * (*len as usize);
                            (self.talloc_n(total), total, Some(*len as usize), *st)
                        }
                    };
                    let l = CellLayout {
                        base,
                        width,
                        ty: scalar_ty,
                        array_len,
                        layout: super::layout::ArrayLayout::Linear,
                    };
                    // Zero the freshly allocated cells so the binding starts
                    // as if memset-to-0, matching the BF interpreter's clean
                    // tape assumption at top level.
                    for i in 0..width {
                        self.clear(base + i);
                    }
                    self.scopes
                        .last_mut()
                        .unwrap()
                        .insert(name.clone(), l.clone());
                    l
                } else {
                    self.lookup(name)
                };
                if let Some(expr) = init {
                    let (v, vw) = self.eval_expr(expr);
                    let v = if vw < layout.width {
                        self.widen(v, vw, layout.width)
                    } else {
                        v
                    };
                    self.bfmove_n(v, layout.base, layout.width);
                    self.tfree_n(v, layout.width);
                }
            }
            Stmt::Assign { lval, expr } => match lval {
                LValue::Var(name) => {
                    let layout = self.lookup(name);
                    let (v, vw) = self.eval_expr(expr);
                    let v = if vw < layout.width {
                        self.widen(v, vw, layout.width)
                    } else {
                        v
                    };
                    self.bfmove_n(v, layout.base, layout.width);
                    self.tfree_n(v, layout.width);
                }
                LValue::Index(name, idx_expr) => {
                    let layout = self.lookup(name);
                    let (v, vw) = self.eval_expr(expr);
                    self.arr_write(&layout, idx_expr, v, vw);
                }
            },
            Stmt::While { cond, body } => self.gen_while(cond, body),
            Stmt::If { cond, then_, else_ } => self.gen_if(cond, then_, else_.as_deref()),
            Stmt::Print(expr) => {
                let (v, vw) = self.eval_expr(expr);
                if vw == 1 {
                    self.print_num(v);
                    self.tfree(v);
                } else {
                    self.print_num_n(v, vw);
                    self.tfree_n(v, vw);
                }
                let sp = self.talloc();
                self.setv(sp, 32);
                self.out(sp);
                self.tfree(sp);
            }
            Stmt::Putchar(expr) => {
                let v = self.eval_expr_1(expr);
                self.out(v);
                self.tfree(v);
            }
            Stmt::Setpixel { x, y, color } => {
                let xc = self.eval_expr_1(x);
                let yc = self.eval_expr_1(y);
                let cc = self.eval_expr_1(color);
                let cmd = self.talloc();
                self.setv(cmd, 0xFE);
                self.out(cmd);
                self.tfree(cmd);
                self.out(xc);
                self.tfree(xc);
                self.out(yc);
                self.tfree(yc);
                self.out(cc);
                self.tfree(cc);
            }
            Stmt::Getchar(lval) => {
                match lval {
                    LValue::Var(name) => {
                        let layout = self.lookup(name);
                        self.inp(layout.base);
                        for i in 1..layout.width {
                            self.clear(layout.base + i);
                        }
                    }
                    LValue::Index(name, idx_expr) => {
                        let layout = self.lookup(name);
                        let ew = layout.elem_width();
                        let t = self.talloc_n(ew);
                        self.inp(t);
                        for i in 1..ew {
                            self.clear(t + i);
                        }
                        self.arr_write(&layout, idx_expr, t, ew);
                        // arr_write already calls tfree_n(val, ew) internally
                    }
                }
            }
            Stmt::Call(name, args) => {
                if let Some((r, rw)) = self.inline_call(name, args) {
                    self.tfree_n(r, rw);
                }
            }
            Stmt::Return(expr) => {
                if let Some(e) = expr {
                    let layout = self.lookup(RET_SLOT);
                    let (v, vw) = self.eval_expr(e);
                    let v = if vw < layout.width {
                        self.widen(v, vw, layout.width)
                    } else {
                        v
                    };
                    self.bfmove_n(v, layout.base, layout.width);
                    self.tfree_n(v, layout.width);
                }
                // typeck enforces `return` appears only as the tail of a
                // function body, so no short-circuit machinery is needed —
                // the enclosing `inline_call` returns right after this frame.
            }
            Stmt::Scan(lval) => match lval {
                LValue::Var(name) => {
                    let layout = self.lookup(name);
                    self.scan_num_n(layout.base, layout.width);
                }
                LValue::Index(name, idx_expr) => {
                    let layout = self.lookup(name);
                    let ew = layout.elem_width();
                    let t = self.talloc_n(ew);
                    self.scan_num_n(t, ew);
                    self.arr_write(&layout, idx_expr, t, ew);
                }
            },
        }
    }

    fn gen_while(&mut self, cond: &Expr, body: &[Stmt]) {
        let cc = self.talloc();
        self.clear(cc);
        let sv = self.tsave();
        let (r, rw) = self.eval_expr(cond);
        let cond_val = if rw > 1 {
            let nz = self.is_nz_n(r, rw);
            self.tfree_n(r, rw);
            nz
        } else {
            r
        };
        self.bfmove(cond_val, cc);
        self.tfree(cond_val);
        self.trestore(sv);
        self.used.insert(cc.saturating_sub(self.temp_base()));

        self.goto(cc);
        self.raw("[");
        for s in body {
            self.gen_stmt(s);
        }
        let sv2 = self.tsave();
        let (r2, r2w) = self.eval_expr(cond);
        let cond_val2 = if r2w > 1 {
            let nz = self.is_nz_n(r2, r2w);
            self.tfree_n(r2, r2w);
            nz
        } else {
            r2
        };
        self.clear(cc);
        self.bfmove(cond_val2, cc);
        self.tfree(cond_val2);
        self.trestore(sv2);
        self.used.insert(cc.saturating_sub(self.temp_base()));
        self.goto(cc);
        self.raw("]");
        self.tfree(cc);
    }

    fn gen_if(&mut self, cond: &Expr, then_: &[Stmt], else_: Option<&[Stmt]>) {
        let sv = self.tsave();
        let cc = self.talloc();
        let ef = self.talloc();
        self.clear(cc);
        let (r, rw) = self.eval_expr(cond);
        let cond_val = if rw > 1 {
            let nz = self.is_nz_n(r, rw);
            self.tfree_n(r, rw);
            nz
        } else {
            r
        };
        self.bfmove(cond_val, cc);
        self.tfree(cond_val);
        self.trestore(sv);
        self.used.insert(cc.saturating_sub(self.temp_base()));
        self.used.insert(ef.saturating_sub(self.temp_base()));

        self.setv(ef, 1);
        self.goto(cc);
        self.raw("[");
        self.clear(ef);
        for s in then_ {
            self.gen_stmt(s);
        }
        self.clear(cc);
        self.goto(cc);
        self.raw("]");
        if let Some(else_body) = else_ {
            self.goto(ef);
            self.raw("[");
            for s in else_body {
                self.gen_stmt(s);
            }
            self.clear(ef);
            self.goto(ef);
            self.raw("]");
        }
        self.tfree(ef);
        self.tfree(cc);
    }
}
