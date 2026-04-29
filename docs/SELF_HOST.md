# BF Self-Hosting Interpreter

[`examples/bf_self_host.bfs`](../examples/bf_self_host.bfs) is a Brainfuck
interpreter written entirely in BFS (the AmazingBF Brainf Script DSL).
`bfsc` lowers it to Brainfuck text, which AmazingBF then runs — **BF
interpreting BF**.

The reference design is the LINUX DO post _"编码尝试：使用 Brainfuck
实现 Brainfuck 解释器"_, which builds the same target via a custom C
subset (`bfrtc`). The implementation here doesn't follow that pipeline
verbatim — BFS already provides typed scalars, arrays, and structured
control flow — but the performance ideas (compact opcode encoding,
pre-computed bracket targets, hot-path-first dispatch) carry over.

## Pipeline

```text
examples/bf_self_host.bfs
   │ bfsc                      (BFS → BF text, ~13 MB)
   ▼
bf_self_host.bf
   │ AmazingBF (interpret | jit | tiered | compile)
   ▼
runs the user's BF program from stdin
```

## I/O convention

Stdin to the compiled interpreter:

```text
<bf_program>!<bf_program_input>
```

The `!` byte (or EOF, byte `0xFF`) terminates the program section. Bytes
after `!` feed the user program's `,` operator. This matches the
"merge program + input on a single tape" trick from the reference post.

Run a BF program through it:

```bash
cargo build --release
./target/release/bfsc examples/bf_self_host.bfs -o /tmp/bfi.bf

# Pipe a BF source plus input
printf '%s!%s' "$(cat tests/cases/1.bf | tr -d '\n')" '' \
  | ./target/release/AmazingBF /tmp/bfi.bf -q
# → Hello World!
```

For one-shot use, `bfsc -c` jumps straight to a native executable:

```bash
./target/release/bfsc examples/bf_self_host.bfs -c -O3 -o /tmp/bfi
printf '%s!' "$(cat tests/cases/1.bf | tr -d '\n')" | /tmp/bfi
```

## Design

### Memory layout

All four buffers are u8 arrays sized `256` (or `32`), the largest
random-access size that fits inside `bfsc`'s u8 array indexing today:

| Buffer | Width | Purpose                            |
|--------|-------|------------------------------------|
| `prog` | 256   | encoded opcode stream (1‥8)        |
| `jmp`  | 256   | bracket jump table (target index)  |
| `tape` | 256   | simulated BF tape                  |
| `bstk` | 32    | bracket stack used during pre-pass |

Sentinel: `prog[prog_len]` is left at `0`, so falling off the end of the
program lands on opcode `0` and exits the dispatch loop cleanly. No
explicit length check inside the hot loop.

### Compact opcode encoding

The eight Brainfuck operators are remapped at load time:

| BF char | Opcode | Why this number                    |
|---------|--------|------------------------------------|
| `>`     | 1      | most common, hits dispatch first   |
| `<`     | 2      |                                    |
| `+`     | 3      |                                    |
| `-`     | 4      |                                    |
| `.`     | 5      |                                    |
| `,`     | 6      |                                    |
| `[`     | 7      |                                    |
| `]`     | 8      |                                    |

Equality checks against small constants are dramatically cheaper in BF
than against ASCII codes (each `==` literal becomes ~`val` `+`/`-`
characters in the emitted code), so dispatch shrinks by roughly 6×
versus matching the raw `+`,`-`,`>`,… bytes.

Opcode `0` is reserved as the halt sentinel.

### Bracket pre-pass

A single linear walk over `prog` populates `jmp` so that
`jmp[i_open] = i_close` and `jmp[i_close] = i_open`, using `bstk` as a
stack of pending `[` positions. After this pass, every `[` and `]` jump
in the run loop is O(1) — no scan, no nested counting. This matches
the `boot.bf` "re-encode for speed" idea from the reference post.

### Hot-path dispatch

The execution loop is a most-frequent-first if/else cascade. Because
BFS lowers `if/else` into nested BF brackets that already short-circuit,
the common-case path (`>`, `<`, `+`, `-`) runs only one or two
comparisons before reaching its body. Backwards branches are dispatched
in the same chain — only `[` and `]` perform an extra `tape[ptr]` read
plus a `jmp[pc]` lookup.

### Why not bigger buffers?

`bfsc::codegen::eval_expr_1` truncates array indices to one byte, so
arrays beyond 256 elements would silently alias their first 256 cells.
Lifting that cap would mean teaching `arr_read` / `arr_write` to widen
their indexing arithmetic, then routing `eval_expr` through the
appropriate width — a contained but real change. Until that lands, the
self-host is bounded to programs ≤ 255 instructions on a 256-cell tape.

## Limits

- `prog_len ≤ 255` — bigger programs are silently truncated by the
  reader. Several canonical BF benchmarks (e.g. `mandelbrot.bf`,
  `dbfi.bf`) overflow this and are not supported until the bfsc array
  cap is raised.
- `tape_size = 256` — programs that scan past cell 255 wrap on `ptr`.
- Bracket nesting ≤ 32. The pre-pass overruns `bstk` if exceeded; this
  is _not_ checked. Real BF programs rarely come close.
- Bracket balance is assumed; an unmatched `]` underflows `bsp`.

## Performance snapshot

Recorded with `cargo build --release` on the same machine that ran the
existing benchmark suite (Intel Core i9-14900K, Linux). The interpreter
BF text is ~13 MB; about ~2.4 s of every interpreted run is spent
parsing it into HIR.

| Mode                                            | `tests/cases/1.bf` (Hello World, 106 B) |
|-------------------------------------------------|------------------------------------------|
| `AmazingBF -m interpret` on the BFS-emitted BF  | ~2.5 s                                   |
| `AmazingBF -m jit`        on the BFS-emitted BF | ~2.7 s                                   |
| `AmazingBF -m compile -O3` (BF → x86_64) + run  | ~3.3 s compile + 0.03 s run              |
| `bfsc -c -O3` direct to native                  | one-shot equivalent of the line above    |

The interpret/JIT numbers are dominated by parse cost, not execution —
the inner loop itself is far below 0.1 s for `1.bf`. Bigger BF inputs
within the 255-instruction budget (e.g. `5.bf`, 242 B) push the
execution component up to ~0.7 s under interpret, while staying at
~0.06 s natively.

## Test

`tests/self_host.rs` compiles the BFS source, then for every BF fixture
under `tests/cases/*.bf` whose filtered length fits the budget, feeds
`<program>!<input>` into the compiled interpreter under `AmazingBF -q`
and diffs against the matching `.out`. Cases over the budget are
skipped, not failed, so the test stays meaningful as the limit grows.
