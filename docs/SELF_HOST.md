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

> 中文同伴文档位于 [`docs/SELF_HOST_CN.md`](SELF_HOST_CN.md)，与本文件保持同步。

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

All five buffers are u8 arrays sized so `bfsc`'s cheap 1-byte indexing
path stays active (`arr_len ≤ 256`). Declaration order puts the
hot arrays nearest the temp pool — emitted gotos shrink accordingly:

| Buffer | Length | Purpose                                                  |
|--------|--------|----------------------------------------------------------|
| `bstk` |     64 | bracket stack used during the Phase 2 pre-pass           |
| `jmp`  |    256 | bracket targets (post-fold PC)                           |
| `prog` |    256 | encoded opcode stream (0 sentinel past `prog_len`)       |
| `cnt`  |    256 | RLE counts paralleling `prog` (used by ops 1‥4)          |
| `tape` |    256 | simulated BF tape                                        |

Sentinel: `prog[prog_len]` is left at `0`, so falling off the end of the
program lands on opcode `0` and exits the dispatch loop cleanly. No
explicit length check inside the hot loop. Phase 1b explicitly writes
`prog[w] = 0` after fold so collapsed instructions don't leave stale
opcodes behind.

### Compact opcode encoding

The eight Brainfuck operators map onto a 1..8 alphabet at load time, and
three more super-opcodes (9..11) recognise common idioms:

| BF / idiom        | Opcode | Notes                                  |
|-------------------|--------|----------------------------------------|
| `>`               | 1      | RLE — `cnt[pc]` consecutive steps      |
| `<`               | 2      | RLE                                    |
| `+`               | 3      | RLE — `tape[ptr] += cnt[pc]`           |
| `-`               | 4      | RLE — `tape[ptr] -= cnt[pc]`           |
| `.`               | 5      |                                        |
| `,`               | 6      |                                        |
| `[`               | 7      |                                        |
| `]`               | 8      |                                        |
| `[-]` / `[+]`     | 9      | Zero — replaces the body loop entirely |
| `[<]`             | 10     | ScanLeft  — tight inner loop           |
| `[>]`             | 11     | ScanRight                              |

Opcode `0` is reserved as the halt sentinel.

Equality checks against small constants are dramatically cheaper in BF
than against ASCII codes (each `==` literal costs ~`val` `+`/`-`
characters), so the dispatch chain on opcodes 1..11 shrinks by roughly
6× versus matching the raw bytes.

### Phase 1a — read with inline RLE

The reader keeps the most recent emitted opcode in scope. When the
incoming character maps to one of `>/</+/-` and matches the previous
opcode (and `cnt[prog_len-1] < 255`), the count is bumped instead of
emitting a new pair. Otherwise a fresh `(op, cnt=1)` is appended.
Long `+++++…` runs that previously consumed a slot per character now
consume one — the practical program-size budget grows accordingly.

### Phase 1b — pattern fold

A single two-pointer pass collapses the 3-element window `[ X ]` where
`X` has `cnt == 1` and `X ∈ {Add(1), Sub(1), Right(1), Left(1)}`:

- `[ Add(1) ]` / `[ Sub(1) ]` → `Zero` (9)
- `[ Right(1) ]` → `ScanRight` (11)
- `[ Left(1) ]` → `ScanLeft` (10)

Phase 1b runs *before* Phase 2 so bracket targets reflect the post-fold
indices. The collapsed loops execute as a single dispatch instead of
the 3-instruction loop body that classical BF interpreters would walk.

### Phase 2 — bracket pre-pass

A linear walk over `prog` populates `jmp` so `jmp[i_open] = i_close` and
`jmp[i_close] = i_open`, using `bstk` as the open-`[` stack. Every
`[`/`]` jump in the run loop becomes O(1).

### Phase 3 — hot-path dispatch

The execution loop is a most-frequent-first if/else cascade. The
RLE-aware arms (1..4) read `cnt[pc]` once and apply the run in a single
BFS statement. The pattern arms (9..11) replace the inner loop their
input was originally written as. Anything unrecognised falls through to
the halt branch.

### Why not bigger buffers?

`bfsc::codegen::arr_read` / `arr_write` now switch to a 2-byte index
path when `arr_len > 256`, so the buffers can technically grow. In
practice, the 2-byte path emits substantially more BF text — a 384-cell
self-host produced ~5 GB of BF on a quick experiment, which is
impractical to parse and run. The 256-cell budget paired with RLE+fold
typically supports 500-1200 BF source characters, well past the
pre-RLE 255-char ceiling, and keeps the emitted BF under ~30 MB.

## Limits

- **`prog_len ≤ 254` post-fold.** Programs are RLE+fold-encoded; the
  effective limit on raw BF source depends on how compressible it is.
  Highly redundant programs (lots of `+++…`, `[-]` cells, `[<]`/`[>]`
  scans) can exceed 1000 source bytes and still fit.
- `tape_size = 256` — programs that scan past cell 255 wrap on `ptr`.
- Bracket nesting ≤ 64. The pre-pass overruns `bstk` if exceeded; this
  is _not_ checked. Real BF programs rarely come close.
- Bracket balance is assumed; an unmatched `]` underflows `bsp`.
- Standard benchmarks `dbfi.b` (~294 ops post-fold), `factor.b` (~1206),
  `mandelbrot.b` (~3867), `hanoi.b` (~14863) still overflow the budget
  and are not supported until the buffer arrays grow further.

## Performance snapshot

Recorded with `cargo build --release` on the same machine as the main
benchmark suite (Intel Core i9-14900K, Linux). The new BFS-emitted BF
text is ~26 MB (vs. the pre-RLE ~13 MB); about half of every interpreted
run is parse cost, the rest dispatch.

| Mode                                            | `tests/cases/1.bf` (Hello World, 106 B) |
|-------------------------------------------------|------------------------------------------|
| `AmazingBF -m interpret` on the BFS-emitted BF  | ~10 s                                    |
| `AmazingBF -m compile -O3` (BF → x86_64) + run  | ~12 s compile + 0.03 s run               |

The Hello-World figure is dominated by parse cost — RLE/pattern wins
appear on programs that exercise long runs or `[-]`/`[<]`/`[>]`
idioms. A 792-byte synthetic mix of `[-]` and `+++…` runs (way past the
old 255-char ceiling) executes correctly under the new self-host where
the previous one silently truncated.

## Test

`tests/self_host.rs` compiles the BFS source, then for every BF fixture
under `tests/cases/*.bf` whose post-fold opcode count fits the budget
(`effective_op_count ≤ 254`), feeds `<program>!<input>` into the
compiled interpreter under `AmazingBF -q` and diffs against the
matching `.out`. Cases over the budget are skipped, not failed, so the
suite stays meaningful as the buffers grow.
