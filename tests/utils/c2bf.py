#!/usr/bin/env python3
"""
c2bf.py — 简易 C 子集 → Brainfuck 编译器 (v2)
================================================
支持的 C 子集语法:
  - int 变量声明:       int x;
  - int 数组声明:       int arr[10];
  - 赋值:               x = expr;
  - 数组赋值:           arr[expr] = expr;
  - while 循环:         while (expr relop expr) { ... }
  - if / else:          if (expr relop expr) { ... } else { ... }
  - 数字输入:           scan(x);  scan(arr[expr]);
  - 数字输出:           print(expr);
  - 字符输出:           print_char(expr);
  - 算术:               + - * / %
  - 关系:               < > <= >= == !=
  - void main() { ... }

限制:
  - 所有值为 8 位无符号 (0-255), 确保中间值不溢出
  - 数组动态下标通过编译期展开, 最大 20 元素
  - 不支持函数 / 指针 / 结构体 / for 循环

用法:
  python3 c2bf.py input.c -o output.bf
  python3 c2bf.py input.c              # 输出到 stdout
"""

import sys, re

# ============================================================
#  LEXER
# ============================================================

TOKEN_SPEC = [
    ('MCOMMENT', r'/\*[\s\S]*?\*/'),
    ('COMMENT',  r'//[^\n]*'),
    ('NUMBER',   r'\d+'),
    ('IDENT',    r'[A-Za-z_]\w*'),
    ('LE', r'<='), ('GE', r'>='), ('EQ', r'=='), ('NE', r'!='),
    ('LT', r'<'),  ('GT', r'>'),  ('ASSIGN', r'='),
    ('PLUS', r'\+'), ('MINUS', r'-'), ('STAR', r'\*'),
    ('SLASH', r'/'), ('PERCENT', r'%'),
    ('LPAREN', r'\('), ('RPAREN', r'\)'),
    ('LBRACK', r'\['), ('RBRACK', r'\]'),
    ('LBRACE', r'\{'), ('RBRACE', r'\}'),
    ('SEMI', r';'), ('COMMA', r','),
    ('SKIP', r'[ \t\r\n]+'),
    ('MISMATCH', r'.'),
]
_tok_re = re.compile('|'.join(f'(?P<{n}>{p})' for n, p in TOKEN_SPEC))
KEYWORDS = {'int','void','while','if','else','scan','print','print_char','main'}

class Token:
    __slots__ = ('type','value','pos')
    def __init__(self, tp, val, pos): self.type=tp; self.value=val; self.pos=pos
    def __repr__(self): return f'Token({self.type},{self.value!r})'

def tokenize(code):
    tokens = []
    for m in _tok_re.finditer(code):
        kind, val = m.lastgroup, m.group()
        if kind in ('SKIP','COMMENT','MCOMMENT'): continue
        if kind == 'MISMATCH': raise SyntaxError(f"Unexpected char {val!r} at {m.start()}")
        if kind == 'IDENT' and val in KEYWORDS: kind = 'KW_' + val.upper()
        if kind == 'NUMBER': val = int(val)
        tokens.append(Token(kind, val, m.start()))
    tokens.append(Token('EOF', None, len(code)))
    return tokens

# ============================================================
#  AST
# ============================================================
class Program:
    def __init__(s, decls, stmts): s.decls=decls; s.stmts=stmts
class VarDecl:
    def __init__(s, name, size=1): s.name=name; s.size=size
class Assign:
    def __init__(s, name, expr): s.name=name; s.expr=expr
class ArrayAssign:
    def __init__(s, name, index, expr): s.name=name; s.index=index; s.expr=expr
class While:
    def __init__(s, cond, body): s.cond=cond; s.body=body
class If:
    def __init__(s, cond, then_body, else_body=None): s.cond=cond; s.then_body=then_body; s.else_body=else_body
class Print:
    def __init__(s, expr): s.expr=expr
class PrintChar:
    def __init__(s, expr): s.expr=expr
class Scan:
    def __init__(s, name, index=None): s.name=name; s.index=index
class BinOp:
    def __init__(s, op, left, right): s.op=op; s.left=left; s.right=right
class Cmp:
    def __init__(s, op, left, right): s.op=op; s.left=left; s.right=right
class Var:
    def __init__(s, name): s.name=name
class ArrayAccess:
    def __init__(s, name, index): s.name=name; s.index=index
class Num:
    def __init__(s, value): s.value=value

# ============================================================
#  PARSER
# ============================================================
class Parser:
    def __init__(s, tokens): s.tokens=tokens; s.pos=0
    def cur(s): return s.tokens[s.pos]
    def peek(s, tp): return s.cur().type == tp
    def expect(s, tp):
        t = s.cur()
        if t.type != tp: raise SyntaxError(f"Expected {tp}, got {t} at pos {t.pos}")
        s.pos += 1; return t
    def match(s, tp):
        if s.peek(tp): s.pos+=1; return True
        return False

    def parse(s):
        decls = []
        while s.peek('KW_INT'): decls.extend(s._decls())
        s.expect('KW_VOID'); s.expect('KW_MAIN')
        s.expect('LPAREN'); s.expect('RPAREN'); s.expect('LBRACE')
        stmts = s._stmt_list()
        s.expect('RBRACE')
        return Program(decls, stmts)

    def _decls(s):
        s.expect('KW_INT'); result=[]
        while True:
            name = s.expect('IDENT').value
            if s.match('LBRACK'):
                sz = s.expect('NUMBER').value; s.expect('RBRACK')
                result.append(VarDecl(name, sz))
            else: result.append(VarDecl(name))
            if not s.match('COMMA'): break
        s.expect('SEMI'); return result

    def _stmt_list(s):
        stmts = []
        while not s.peek('RBRACE') and not s.peek('EOF'): stmts.append(s._stmt())
        return stmts

    def _stmt(s):
        if s.peek('KW_WHILE'):     return s._while()
        if s.peek('KW_IF'):        return s._if()
        if s.peek('KW_PRINT_CHAR'):return s._print_char()
        if s.peek('KW_PRINT'):     return s._print()
        if s.peek('KW_SCAN'):      return s._scan()
        if s.peek('LBRACE'):
            s.expect('LBRACE'); r=s._stmt_list(); s.expect('RBRACE'); return r
        return s._assign()

    def _while(s):
        s.expect('KW_WHILE'); s.expect('LPAREN'); c=s._cond(); s.expect('RPAREN')
        s.expect('LBRACE'); b=s._stmt_list(); s.expect('RBRACE')
        return While(c,b)

    def _if(s):
        s.expect('KW_IF'); s.expect('LPAREN'); c=s._cond(); s.expect('RPAREN')
        s.expect('LBRACE'); th=s._stmt_list(); s.expect('RBRACE')
        el=None
        if s.match('KW_ELSE'):
            s.expect('LBRACE'); el=s._stmt_list(); s.expect('RBRACE')
        return If(c,th,el)

    def _print(s):
        s.expect('KW_PRINT'); s.expect('LPAREN'); e=s._expr()
        s.expect('RPAREN'); s.expect('SEMI'); return Print(e)

    def _print_char(s):
        s.expect('KW_PRINT_CHAR'); s.expect('LPAREN'); e=s._expr()
        s.expect('RPAREN'); s.expect('SEMI'); return PrintChar(e)

    def _scan(s):
        s.expect('KW_SCAN'); s.expect('LPAREN'); name=s.expect('IDENT').value
        idx=None
        if s.match('LBRACK'): idx=s._expr(); s.expect('RBRACK')
        s.expect('RPAREN'); s.expect('SEMI'); return Scan(name,idx)

    def _assign(s):
        name = s.expect('IDENT').value
        if s.match('LBRACK'):
            idx=s._expr(); s.expect('RBRACK'); s.expect('ASSIGN')
            val=s._expr(); s.expect('SEMI'); return ArrayAssign(name,idx,val)
        s.expect('ASSIGN'); val=s._expr(); s.expect('SEMI'); return Assign(name,val)

    def _cond(s):
        left=s._expr()
        ops={'LT':'<','GT':'>','LE':'<=','GE':'>=','EQ':'==','NE':'!='}
        t=s.cur()
        if t.type in ops: s.pos+=1; return Cmp(ops[t.type], left, s._expr())
        raise SyntaxError(f"Expected comparison op, got {t}")

    def _expr(s):
        n=s._term()
        while s.cur().type in ('PLUS','MINUS'):
            op='+' if s.cur().type=='PLUS' else '-'; s.pos+=1
            n=BinOp(op,n,s._term())
        return n

    def _term(s):
        n=s._factor()
        while s.cur().type in ('STAR','SLASH','PERCENT'):
            op={'STAR':'*','SLASH':'/','PERCENT':'%'}[s.cur().type]; s.pos+=1
            n=BinOp(op,n,s._factor())
        return n

    def _factor(s):
        t=s.cur()
        if t.type=='NUMBER': s.pos+=1; return Num(t.value)
        if t.type=='LPAREN': s.pos+=1; e=s._expr(); s.expect('RPAREN'); return e
        if t.type=='IDENT':
            name=t.value; s.pos+=1
            if s.match('LBRACK'): idx=s._expr(); s.expect('RBRACK'); return ArrayAccess(name,idx)
            return Var(name)
        raise SyntaxError(f"Unexpected token {t} in expression")


# ============================================================
#  BRAINFUCK CODE GENERATOR  (set-based temp allocator)
# ============================================================

class BFGen:
    def __init__(self):
        self.code      = []
        self.ptr       = 0
        self.vars      = {}
        self.arrays    = {}
        self.next_cell = 0
        self.temp_base = 0
        self.used      = set()      # temp indices in use
        self.max_temp  = 0

    # --- memory layout ---
    def alloc_var(self, name):
        self.vars[name] = self.next_cell; self.next_cell += 1
    def alloc_array(self, name, size):
        self.arrays[name] = (self.next_cell, size); self.next_cell += size
    def finalize(self):
        self.temp_base = self.next_cell

    # --- set-based temp allocator ---
    def talloc(self):
        i = 0
        while i in self.used: i += 1
        self.used.add(i)
        if i+1 > self.max_temp: self.max_temp = i+1
        return self.temp_base + i

    def tfree(self, cell):
        i = cell - self.temp_base
        self.used.discard(i)

    def tsave(self):   return frozenset(self.used)
    def trestore(self, s): self.used = set(s)

    # --- BF primitives ---
    def emit(self, s):  self.code.append(s)
    def goto(self, c):
        d = c - self.ptr
        if d > 0:   self.emit('>'*d)
        elif d < 0: self.emit('<'*(-d))
        self.ptr = c
    def clear(self, c):    self.goto(c); self.emit('[-]')
    def inc(self, c, n=1):
        if n>0: self.goto(c); self.emit('+'*n)
    def dec(self, c, n=1):
        if n>0: self.goto(c); self.emit('-'*n)
    def setv(self, c, v):  self.clear(c); self.inc(c, v%256) if v>0 else None
    def inp(self, c):      self.goto(c); self.emit(',')
    def out(self, c):      self.goto(c); self.emit('.')

    def copy(self, src, dst):
        tmp = self.talloc()
        self.clear(dst); self.clear(tmp)
        self.goto(src); self.emit('[')
        self.goto(dst); self.emit('+')
        self.goto(tmp); self.emit('+')
        self.goto(src); self.emit('-]')
        self.goto(tmp); self.emit('[')
        self.goto(src); self.emit('+')
        self.goto(tmp); self.emit('-]')
        self.tfree(tmp)

    def move(self, src, dst):
        self.clear(dst)
        self.goto(src); self.emit('[')
        self.goto(dst); self.emit('+')
        self.goto(src); self.emit('-]')

    def add_to(self, src, dst):
        self.goto(src); self.emit('[')
        self.goto(dst); self.emit('+')
        self.goto(src); self.emit('-]')

    def sub_from(self, src, dst):
        self.goto(src); self.emit('[')
        self.goto(dst); self.emit('-')
        self.goto(src); self.emit('-]')

    # ---- is nonzero (non-destructive) ----
    def is_nz(self, cell):
        r  = self.talloc(); self.clear(r)
        tc = self.talloc()
        self.copy(cell, tc)
        self.goto(tc); self.emit('[')
        self.setv(r, 1)
        self.clear(tc)
        self.goto(tc); self.emit(']')
        self.tfree(tc)
        return r

    # ---- if / else ----
    def if_else(self, cond, then_fn, else_fn=None):
        """cond is consumed."""
        if else_fn is None:
            self.goto(cond); self.emit('[')
            then_fn()
            self.clear(cond); self.goto(cond); self.emit(']')
            return
        ef = self.talloc(); self.setv(ef, 1)
        self.goto(cond); self.emit('[')
        self.clear(ef)
        then_fn()
        self.clear(cond); self.goto(cond); self.emit(']')
        self.goto(ef); self.emit('[')
        else_fn()
        self.clear(ef); self.goto(ef); self.emit(']')
        self.tfree(ef)

    # ============================================================
    #  COMPARISON  a > b  (a,b are temp cells, CONSUMED)
    # ============================================================
    def cmp_gt(self, a, b):
        result = self.talloc(); self.clear(result)
        flag   = self.talloc(); self.setv(flag, 1)

        self.goto(flag); self.emit('[')     # while flag

        bnz = self.is_nz(b)

        def _b_pos():
            self.dec(b)
            anz = self.is_nz(a)
            def _a_pos(): self.dec(a)       # both > 0, continue (flag=1)
            def _a_zero(): self.clear(flag) # a exhausted → a ≤ b
            self.if_else(anz, _a_pos, _a_zero)
            self.tfree(anz)

        def _b_zero():
            anz2 = self.is_nz(a)
            def _yes(): self.setv(result, 1)
            self.if_else(anz2, _yes)
            self.tfree(anz2)
            self.clear(flag)

        self.if_else(bnz, _b_pos, _b_zero)
        self.tfree(bnz)

        self.goto(flag); self.emit(']')
        self.tfree(flag); self.tfree(a); self.tfree(b)
        return result

    def cmp_lt(self, a, b):  return self.cmp_gt(b, a)

    def cmp_ge(self, a, b):
        r = self.cmp_gt(b, a)
        return self.negate(r)

    def cmp_le(self, a, b):
        r = self.cmp_gt(a, b)
        return self.negate(r)

    def cmp_eq(self, a, b):
        self.sub_from(b, a); self.tfree(b)
        return self.negate(a)       # a==0 → 1

    def cmp_ne(self, a, b):
        r = self.cmp_eq(a, b)
        return self.negate(r)

    def negate(self, c):
        """In-place: c = (c==0)?1:0. Returns same cell."""
        t = self.talloc()
        self.move(c, t)             # t=old, c=0
        self.inc(c)                 # c=1
        self.goto(t); self.emit('[')
        self.clear(c)               # was nonzero → 0
        self.clear(t)
        self.goto(t); self.emit(']')
        self.tfree(t)
        return c

    # ============================================================
    #  ARITHMETIC
    # ============================================================
    def arith_add(self, a, b):
        self.add_to(b, a); self.tfree(b); return a

    def arith_sub(self, a, b):
        self.sub_from(b, a); self.tfree(b); return a

    def arith_mul(self, a, b):
        r  = self.talloc(); self.clear(r)
        tc = self.talloc()
        self.goto(a); self.emit('[')
        self.dec(a)
        self.copy(b, tc)
        self.add_to(tc, r)
        self.goto(a); self.emit(']')
        self.tfree(tc); self.tfree(a); self.tfree(b)
        return r

    def arith_div(self, a, b):
        q = self.talloc(); self.clear(q)
        flag = self.talloc(); self.setv(flag, 1)
        self.goto(flag); self.emit('[')
        self.clear(flag)

        ta = self.talloc(); tb = self.talloc()
        self.copy(a, ta); self.copy(b, tb)
        bga = self.cmp_gt(tb, ta)       # b > a?
        ge  = self.negate(bga)          # a >= b

        def _sub():
            ts = self.talloc()
            self.copy(b, ts)
            self.sub_from(ts, a)
            self.tfree(ts)
            self.inc(q)
            self.setv(flag, 1)
        def _done(): pass

        self.if_else(ge, _sub, _done)
        self.tfree(ge)

        self.goto(flag); self.emit(']')
        self.tfree(flag); self.tfree(a); self.tfree(b)
        return q

    def arith_mod(self, a, b):
        flag = self.talloc(); self.setv(flag, 1)
        self.goto(flag); self.emit('[')
        self.clear(flag)

        ta = self.talloc(); tb = self.talloc()
        self.copy(a, ta); self.copy(b, tb)
        bga = self.cmp_gt(tb, ta)
        ge  = self.negate(bga)

        def _sub():
            ts = self.talloc()
            self.copy(b, ts)
            self.sub_from(ts, a)
            self.tfree(ts)
            self.setv(flag, 1)
        def _done(): pass

        self.if_else(ge, _sub, _done)
        self.tfree(ge)

        self.goto(flag); self.emit(']')
        self.tfree(flag); self.tfree(b)
        return a

    # ============================================================
    #  EXPRESSION / CONDITION EVALUATION
    # ============================================================
    def eval_expr(self, node):
        if isinstance(node, Num):
            t = self.talloc(); self.setv(t, node.value % 256); return t
        if isinstance(node, Var):
            t = self.talloc(); self.copy(self.vars[node.name], t); return t
        if isinstance(node, ArrayAccess):
            return self._arr_read(node.name, node.index)
        if isinstance(node, BinOp):
            a = self.eval_expr(node.left)
            b = self.eval_expr(node.right)
            if node.op=='+': return self.arith_add(a,b)
            if node.op=='-': return self.arith_sub(a,b)
            if node.op=='*': return self.arith_mul(a,b)
            if node.op=='/': return self.arith_div(a,b)
            if node.op=='%': return self.arith_mod(a,b)
        raise ValueError(f"Unknown expr {type(node)}")

    def eval_cond(self, node):
        a = self.eval_expr(node.left)
        b = self.eval_expr(node.right)
        if node.op=='>':  return self.cmp_gt(a,b)
        if node.op=='<':  return self.cmp_lt(a,b)
        if node.op=='>=': return self.cmp_ge(a,b)
        if node.op=='<=': return self.cmp_le(a,b)
        if node.op=='==': return self.cmp_eq(a,b)
        if node.op=='!=': return self.cmp_ne(a,b)
        raise ValueError(f"Unknown cmp {node.op}")

    # ============================================================
    #  ARRAY ACCESS
    # ============================================================
    def _arr_read(self, name, idx_expr):
        base, size = self.arrays[name]
        idx = self.eval_expr(idx_expr)
        result = self.talloc(); self.clear(result)
        for i in range(size):
            ti = self.talloc(); self.copy(idx, ti)
            if i > 0: self.dec(ti, i)
            eq = self.talloc(); self.setv(eq, 1)
            tc = self.talloc(); self.copy(ti, tc)
            self.goto(tc); self.emit('[')
            self.clear(eq); self.clear(tc)
            self.goto(tc); self.emit(']')
            self.tfree(tc)
            self.goto(eq); self.emit('[')
            self.copy(base + i, result)
            self.clear(eq); self.goto(eq); self.emit(']')
            self.tfree(eq); self.tfree(ti)
        self.tfree(idx)
        return result

    def _arr_write(self, name, idx_expr, val):
        base, size = self.arrays[name]
        idx = self.eval_expr(idx_expr)
        for i in range(size):
            ti = self.talloc(); self.copy(idx, ti)
            if i > 0: self.dec(ti, i)
            eq = self.talloc(); self.setv(eq, 1)
            tc = self.talloc(); self.copy(ti, tc)
            self.goto(tc); self.emit('[')
            self.clear(eq); self.clear(tc)
            self.goto(tc); self.emit(']')
            self.tfree(tc)
            self.goto(eq); self.emit('[')
            self.clear(base + i)
            self.copy(val, base + i)
            self.clear(eq); self.goto(eq); self.emit(']')
            self.tfree(eq); self.tfree(ti)
        self.tfree(idx); self.tfree(val)

    # ============================================================
    #  NUMBER I/O
    # ============================================================
    def print_num(self, cell):
        """Print cell (0-255) as decimal. Non-destructive."""
        v = self.talloc(); self.copy(cell, v)
        h = self.talloc(); self.clear(h)
        t = self.talloc(); self.clear(t)
        self._divconst(v, 100, h)
        self._divconst(v, 10, t)

        # Print hundreds (if > 0)
        hp = self.is_nz(h)
        def _ph():
            d = self.talloc(); self.copy(h, d); self.inc(d, 48); self.out(d); self.tfree(d)
        self.if_else(hp, _ph); self.tfree(hp)

        # Print tens (if hundreds > 0 or tens > 0)
        ht = self.talloc(); self.clear(ht)
        ch = self.is_nz(h)
        self.goto(ch); self.emit('['); self.setv(ht, 1); self.clear(ch); self.goto(ch); self.emit(']')
        self.tfree(ch)
        ct = self.is_nz(t)
        self.goto(ct); self.emit('['); self.setv(ht, 1); self.clear(ct); self.goto(ct); self.emit(']')
        self.tfree(ct)
        def _pt():
            d = self.talloc(); self.copy(t, d); self.inc(d, 48); self.out(d); self.tfree(d)
        self.if_else(ht, _pt); self.tfree(ht)

        # Always print ones
        self.inc(v, 48); self.out(v)
        self.tfree(t); self.tfree(h); self.tfree(v)

    def _divconst(self, val, div, quot):
        """quot = val / div; val %= div. In-place."""
        self.clear(quot)
        flag = self.talloc(); self.setv(flag, 1)
        self.goto(flag); self.emit('[')
        self.clear(flag)

        tv = self.talloc(); td = self.talloc()
        self.copy(val, tv); self.setv(td, div)
        gt = self.cmp_gt(td, tv)    # div > val?
        ge = self.negate(gt)        # val >= div

        def _sub():
            self.dec(val, div)
            self.inc(quot)
            self.setv(flag, 1)
        def _stop(): pass

        self.if_else(ge, _sub, _stop)
        self.tfree(ge)
        self.goto(flag); self.emit(']')
        self.tfree(flag)

    def scan_num(self, target):
        """Read decimal from stdin into target. Ends on newline/space/EOF."""
        self.clear(target)
        ch = self.talloc(); self.clear(ch); self.inp(ch)
        lf = self._is_digit(ch)

        self.goto(lf); self.emit('[')
        self._mul10(target)
        dg = self.talloc(); self.copy(ch, dg); self.dec(dg, 48)
        self.add_to(dg, target); self.tfree(dg)
        self.clear(ch); self.inp(ch)
        self.clear(lf)
        lf2 = self._is_digit(ch)
        self.move(lf2, lf); self.tfree(lf2)
        self.goto(lf); self.emit(']')

        self.tfree(lf); self.tfree(ch)

    def _is_digit(self, ch):
        """Return temp=1 if ch is '0'-'9'. Non-destructive."""
        r = self.talloc(); self.clear(r)
        c1 = self.is_nz(ch)
        t10 = self.talloc(); self.copy(ch, t10); self.dec(t10, 10)
        c2 = self.is_nz(t10); self.tfree(t10)
        t32 = self.talloc(); self.copy(ch, t32); self.dec(t32, 32)
        c3 = self.is_nz(t32); self.tfree(t32)
        # r = c1 AND c2 AND c3
        self.goto(c1); self.emit('[')
        self.goto(c2); self.emit('[')
        self.goto(c3); self.emit('[')
        self.setv(r, 1)
        self.clear(c3); self.goto(c3); self.emit(']')
        self.clear(c2); self.goto(c2); self.emit(']')
        self.clear(c1); self.goto(c1); self.emit(']')
        self.tfree(c3); self.tfree(c2); self.tfree(c1)
        return r

    def _mul10(self, cell):
        tc = self.talloc(); self.copy(cell, tc); self.clear(cell)
        for _ in range(10):
            t2 = self.talloc(); self.copy(tc, t2)
            self.add_to(t2, cell); self.tfree(t2)
        self.tfree(tc)

    # ============================================================
    #  STATEMENTS
    # ============================================================
    def gen_stmt(self, node):
        if isinstance(node, list):
            for s in node: self.gen_stmt(s); return
        if isinstance(node, Assign):
            v = self.eval_expr(node.expr); self.move(v, self.vars[node.name]); self.tfree(v); return
        if isinstance(node, ArrayAssign):
            v = self.eval_expr(node.expr); self._arr_write(node.name, node.index, v); return
        if isinstance(node, While):  self._gen_while(node); return
        if isinstance(node, If):     self._gen_if(node); return
        if isinstance(node, Print):
            v = self.eval_expr(node.expr); self.print_num(v); self.tfree(v)
            sp = self.talloc(); self.setv(sp, 32); self.out(sp); self.tfree(sp); return
        if isinstance(node, PrintChar):
            v = self.eval_expr(node.expr); self.out(v); self.tfree(v); return
        if isinstance(node, Scan):
            if node.index is None:
                self.scan_num(self.vars[node.name])
            else:
                t = self.talloc(); self.scan_num(t)
                self._arr_write(node.name, node.index, t)
            return
        raise ValueError(f"Unknown stmt {type(node)}")

    def _gen_while(self, node):
        cc = self.talloc(); self.clear(cc)
        sv = self.tsave()
        r = self.eval_cond(node.cond)
        self.move(r, cc); self.tfree(r)
        self.trestore(sv); self.used.add(cc - self.temp_base)

        self.goto(cc); self.emit('[')
        for s in node.body: self.gen_stmt(s)
        sv2 = self.tsave()
        r2 = self.eval_cond(node.cond)
        self.clear(cc); self.move(r2, cc); self.tfree(r2)
        self.trestore(sv2); self.used.add(cc - self.temp_base)
        self.goto(cc); self.emit(']')
        self.tfree(cc)

    def _gen_if(self, node):
        sv = self.tsave()
        cc = self.talloc(); ef = self.talloc()
        self.clear(cc)
        r = self.eval_cond(node.cond)
        self.move(r, cc); self.tfree(r)
        self.trestore(sv)
        self.used.add(cc - self.temp_base)
        self.used.add(ef - self.temp_base)

        self.setv(ef, 1)
        self.goto(cc); self.emit('[')
        self.clear(ef)
        for s in node.then_body: self.gen_stmt(s)
        self.clear(cc); self.goto(cc); self.emit(']')

        if node.else_body:
            self.goto(ef); self.emit('[')
            for s in node.else_body: self.gen_stmt(s)
            self.clear(ef); self.goto(ef); self.emit(']')

        self.tfree(ef); self.tfree(cc)

    # ============================================================
    #  TOP LEVEL
    # ============================================================
    def compile(self, prog):
        for d in prog.decls:
            if d.size == 1: self.alloc_var(d.name)
            else:           self.alloc_array(d.name, d.size)
        self.finalize()
        for s in prog.stmts: self.gen_stmt(s)
        return ''.join(self.code)


# ============================================================
#  MAIN
# ============================================================
def main():
    import argparse
    ap = argparse.ArgumentParser(description='C-subset → Brainfuck compiler')
    ap.add_argument('input', help='Input .c file')
    ap.add_argument('-o', '--output', help='Output .bf file (default: stdout)')
    ap.add_argument('--stats', action='store_true', help='Print compilation stats')
    args = ap.parse_args()

    with open(args.input) as f: source = f.read()
    prog = Parser(tokenize(source)).parse()
    gen  = BFGen()
    bf   = gen.compile(prog)

    if args.output:
        with open(args.output, 'w') as f: f.write(bf)
        print(f"[OK] {len(bf)} BF instructions → {args.output}")
    else:
        print(bf)

    if args.stats:
        e = sys.stderr
        print(f"\n--- Stats ---", file=e)
        print(f"Variables : {gen.vars}", file=e)
        print(f"Arrays    : {gen.arrays}", file=e)
        print(f"Temp base : {gen.temp_base}", file=e)
        print(f"Max temps : {gen.max_temp}", file=e)
        print(f"Total cells: {gen.temp_base + gen.max_temp}", file=e)
        print(f"BF length : {len(bf)}", file=e)

if __name__ == '__main__':
    main()
