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
        let base = layout.base;
        let arr_len = layout.array_len();
        let ew = layout.elem_width();
        let idx = self.eval_expr_1(idx_expr);
        let result = self.talloc_n(ew);
        for k in 0..ew {
            self.clear(result + k);
        }
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
        (result, ew)
    }

    fn arr_write(
        &mut self,
        layout: &super::layout::CellLayout,
        idx_expr: &Expr,
        val: usize,
        val_w: usize,
    ) {
        let base = layout.base;
        let arr_len = layout.array_len();
        let ew = layout.elem_width();
        // Widen or truncate val to match element width
        let val = if val_w < ew {
            self.widen(val, val_w, ew)
        } else {
            val
        };
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
        self.tfree_n(val, ew);
    }

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
