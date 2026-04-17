# AmazingBF [简体中文](./docs/README_CN.md)

`AmazingBF` is a Brainfuck toolchain project written in Rust. It currently provides two executable paths:

* **Interpretation**: parses the source code and runs it on HIR
* **Native compilation**: compiles LIR into x86_64 native executables (Linux ELF and Windows PE64 backends implemented)

It also includes `bfsc`, a compiler for the **BFS (Brainf Script)** domain-specific language that transpiles `.bfs` source to Brainfuck.

Running `cargo build` produces four entry points:

* `AmazingBF` (full CLI with `-m` / `--mode` / `--target`)
* `bf-interpreter` (fixed interpretation mode)
* `bf-compiler` (fixed compilation mode, default target follows build target, also supports cross-compilation via `--target`)
* `bfsc` (BFS → BF compiler)

The second and third are **specifically designed for ten-line code benchmarks**, and behave the same as `AmazingBF -m interpret -q` and `AmazingBF -m compile`.

---

## Current Capabilities

* Supports basic Brainfuck syntax: `><+-.,[]`
* Provides three execution modes: `interpret`, `compile`, and `dump`
* Complete frontend pipeline: `Lexer -> Parser -> AST`
* Layered intermediate representations: `HIR -> optimize -> LIR`
* Interpreter executes based on `HIR`
* Native backend generates `Linux ELF` or `Windows PE64`, and outputs `.asm` / `.lst` debug files with the same basename as the target
* Optional stderr progress text via `-q` / `-v` flags only (no `tracing` or extra logging crates)

---

## Quick Start

### Requirements

* `Rust stable`
* When using `AmazingBF` in `compile` mode or `bf-compiler`, you can select the target via `--target x86_64-linux|x86_64-windows`; **default follows the build target**
* Currently supported native backends: `x86_64-linux` (ELF) and `x86_64-windows` (PE64)

---

### Build

```bash
cargo build --release
```

The release profile in `Cargo.toml` favors a smaller binary (`opt-level = "z"`, LTO, `strip`, `panic = "abort"`). Runtime dependencies are empty beyond `std`.

---

### Interpretation

```bash
cargo run --bin AmazingBF -- tests/cases/1.bf -q
# or use the specialized entry
cargo run --bin bf-interpreter -- tests/cases/1.bf
```

`AmazingBF` defaults to `interpret` mode. The above example outputs the classic Hello World.

In interpret mode, you can add `--interp-debug` to print tape statistics to **stderr** after execution:
initial/final cell count, accessed index range width, bytes allocated due to right expansion, and total left/right pointer movements.

---

### Compile to Executable

```bash
cargo run --bin AmazingBF -- tests/cases/1.bf -m compile --target x86_64-linux -o hello_bf
# or
cargo run --bin bf-compiler -- tests/cases/1.bf -o hello_bf
./hello_bf
```

`bf-compiler` outputs in the format matching the current build target by default, but also supports explicit cross-compilation:

```bash
cargo run --bin bf-compiler -- tests/cases/1.bf --target x86_64-windows -o hello_bf
```

**When targeting Windows, `.exe` is appended by default.**

During compilation, additional debug artifacts are generated alongside the output:

* `hello_bf.asm`: readable assembly listing
* `hello_bf.lst`: hex listing with offsets

Debug artifact paths follow Rust’s `Path::with_extension()` rules:

* `-o hello_bf` → `hello_bf.asm` / `hello_bf.lst`
* Linux default output `a.out` → `a.asm` / `a.lst`
* Windows default output `a.exe` → `a.asm` / `a.lst`

---

### BFS (Brainf Script) Compiler

`bfsc` compiles `.bfs` source files to Brainfuck text, which can then be run through `AmazingBF`.

**Supported types:**

| Type  | BF cells | Representation                    |
|-------|----------|------------------------------------|
| `u8`  | 1        | wrapping mod 256                  |
| `u16` | 2        | little-endian (lo byte first)     |
| `u32` | 4        | little-endian (lo byte first)     |

**Language features:** variable declarations, arrays, `while`, `if/else`, arithmetic (`+ - * / %`), comparisons (`< > <= >= == !=`), boolean operators (`&& ||`), `scan()`, `print()`, `putchar()`.

```bfs
// Example: bubble sort
let n: u8 = 0;
let arr: [u8; 10];
let i: u8 = 0;

scan(n);
i = 0;
while i < n { scan(arr[i]); i = i + 1; }
// ... sort body ...
```

**Usage:**

```bash
# Compile .bfs to BF text (stdout)
bfsc tests/utils/sort.bfs

# Save BF text to a file, then run it
bfsc tests/utils/sort.bfs -o /tmp/sort.bf
echo "3 2 1" | tr ' ' '\n' | cat <(echo 3) - | AmazingBF /tmp/sort.bf -q

# Compile .bfs directly to a native executable (no intermediate .bf file needed)
bfsc tests/utils/sort.bfs -c -o sort

# Compile for a specific target platform
bfsc tests/utils/sort.bfs -c --target x86_64-linux -o sort

# Pipe directly (only when the BF program does not read stdin)
bfsc tests/utils/linear_eq.bfs | AmazingBF - -q
```

**`bfsc` CLI flags:**

| Flag | Description |
|------|-------------|
| `-o, --output <PATH>` | Without `-c`: write BF text to file (default: stdout). With `-c`: executable output path (default: `a.out` / `a.exe`). |
| `-c, --compile` | Compile all the way to a native x86_64 executable via the AmazingBF backend. Without this flag, only BF text is output. |
| `--target <T>` | Compilation target (only with `-c`): `x86_64-linux` \| `x86_64-windows`. Default follows build target. |
| `-O, --opt-level <0-3>` | Optimization level passed to the AmazingBF backend (only with `-c`, default `3`). |
| `-q, --quiet` | Suppress backend progress messages (only with `-c`). |
| `-h, --help` | Print help. |
| `-V, --version` | Print version. |

> **Note:** When the BF program reads from stdin (e.g. `scan`), always write the compiled BF to a file first and pass it as an argument to `AmazingBF`. Piping both the BF code and program input through stdin simultaneously does not work.

Fixture files for `bfsc` live in `tests/utils/` as `.bfs` / `.in` / `.out` triplets.

---

### Optimization Levels

Set via `-O` / `--opt-level` (default: `0`):

* **O0**: merges consecutive `Move` / `Add` on HIR (single pass), then proceeds normally to LIR → assembly

* **O1**: adds one more HIR pass:

  * `[-]` → `Zero`
  * `[>]` / `[<]` → `Scan`
  * affine loops (e.g. `[->+<]`, `[->>++<<]`) → `LinearMul`
  * simple constant propagation under zero-initial tape assumption
  * removes empty loops `[]` when entry cell is zero
  * peephole simplifications like `Add; Zero → Zero`
  * **does NOT** fold `Zero; Add(k)` into `Add(k)` (semantics differ)

* **O2**: repeats the entire HIR optimization pipeline until reaching a fixed point

* **O3**: same HIR as O2, plus aggressive compile-time folding:

  * no `.` → generate minimal executable with `exit(0)`
  * has `.` but no `,` → interpret at compile time to collect output, then generate a program that directly prints it (`write+exit` on Linux, `WriteFile+ExitProcess` on Windows)

---

### CLI Help

```bash
cargo run -- --help
# short version
cargo run -- -h
```

Man page sources are included:
`man/amazingbf.1` → preview with `man -l man/amazingbf.1`
`man/bfsc.1` → preview with `man -l man/bfsc.1`

---

### Logging

Pipeline messages go to **stderr** as plain text:

* Default: short progress lines (e.g. start/finish, compile summary)
* `-v` / `-vv` / `-vvv`: add more detailed diagnostics
* `-q`: silence those messages (errors from the program under interpretation still use normal stdin/stdout)

There are no `RUST_LOG`-style environment overrides.

---

## Project Architecture

### Pipeline

```text
Brainfuck source
  -> lexer
  -> parser
  -> AST
  -> HIR
  -> optimize
  -> LIR
  -> AsmProgram
  -> machine code
  -> target-specific executable

BFS source (.bfs)
  -> bfsc (lexer -> parser -> typeck -> layout -> codegen)
  -> Brainfuck text
  -> AmazingBF (interpret or compile)
```

Driver behavior:

* **interpret**: runs directly on optimized HIR
* **compile**: continues through LIR → backend → executable
* **dump**: stops at assembly IR, logs only

---

### Module Layout

```text
src/
  lib.rs
  main.rs
  bin/bf-interpreter.rs
  bin/bf-compiler.rs
  bin/bfsc.rs
  cli.rs
  app.rs
  logging.rs
  error.rs
  driver/
  frontend/
  ir/
  interp/
  runtime/
  backend/
  bfsc/          (lexer, parser, typeck, layout, codegen)
tests/
  cases/         (BF fixtures for AmazingBF)
  utils/         (BFS fixtures for bfsc)
  *.rs
```

---

### Key Modules

* `main.rs`: default entry → calls `AmazingBF::run_amazingbf()`
* `app.rs`: CLI parsing, logging init, driver dispatch
* `cli.rs`: minimal argv parser (no `clap`); converts flags into `DriverConfig`
* `logging.rs`: verbosity level from CLI; `log_info` / `log_debug` use `eprintln!`
* `driver/run.rs`: mode dispatch and output handling
* `interp/engine.rs`: HIR interpreter
* `backend/codegen.rs`: LIR → assembly IR
* `backend/x86_64/`: machine code + ELF/PE generation
* `bfsc/`: BFS compiler — `lexer`, `parser`, `typeck`, `layout`, `codegen` modules

---

## Interpreter vs Compiler Boundary

* Interpreter operates on `HIR`
* Backend consumes `LIR`
* `AsmProgram` is backend-internal IR
* Register conventions and memory strategy defined in backend

---

## Testing

### Run Tests

```bash
cargo test
```

Covers backend encoding, ELF/PE structure, and `.lst` consistency.

---

### Test Structure

Located in `tests/cases/`, each case may include:

* `.bf`: program
* `.in`: input (optional)
* `.out`: expected output
* `.md`: description

Test categories:

* `cases_pipeline.rs`: interpreter vs compiler output validation
* `bfsc_pipeline.rs`: BFS compiler end-to-end — `.bfs` → `bfsc` → `AmazingBF` → compare `.out`
* `windows_target.rs`: PE64 structure and cross-compilation
* `compile_pipeline.rs`: slower compile benchmarks (`#[ignore]`)

---

## Appendix

### Sample `.lst` excerpt (`tests/cases/1.bf`, `-O3`, `x86_64-linux`)

At **O3**, programs with `.` but no `,` may be folded into a minimal `write` + `exit` binary. Below is a representative fragment from the generated `.lst` (formatting may vary slightly by toolchain version).

```text
; === Brainfuck x86_64 Hex Listing ===
; 1 instruction(s), 130 bytes encoded

Offset    Hex                                          Assembly
--------- -------------------------------------------- ----------------------------------------
0x0000:   48 83 ec 10                                  sub rsp, 0x10                ; 16-byte stack buffer
          c6 44 24 00 48                               mov byte ptr [rsp+0x00],0x48 ; 'H'
          c6 44 24 01 65                               mov byte ptr [rsp+0x01],0x65 ; 'e'
          c6 44 24 02 6c                               mov byte ptr [rsp+0x02],0x6c ; 'l'
          c6 44 24 03 6c                               mov byte ptr [rsp+0x03],0x6c ; 'l'
          c6 44 24 04 6f                               mov byte ptr [rsp+0x04],0x6f ; 'o'
          c6 44 24 05 20                               mov byte ptr [rsp+0x05],0x20 ; ' '
          c6 44 24 06 57                               mov byte ptr [rsp+0x06],0x57 ; 'W'
          c6 44 24 07 6f                               mov byte ptr [rsp+0x07],0x6f ; 'o'
          c6 44 24 08 72                               mov byte ptr [rsp+0x08],0x72 ; 'r'
          c6 44 24 09 6c                               mov byte ptr [rsp+0x09],0x6c ; 'l'
          c6 44 24 0a 64                               mov byte ptr [rsp+0x0a],0x64 ; 'd'
          c6 44 24 0b 21                               mov byte ptr [rsp+0x0b],0x21 ; '!'
          c6 44 24 0c 0a                               mov byte ptr [rsp+0x0c],0x0a ; '\n'

          48 b8 01 00 00 00 00 00 00 00                mov rax, 1                   ; sys_write
          48 bf 01 00 00 00 00 00 00 00                mov rdi, 1                   ; stdout
          48 89 e6                                     mov rsi, rsp                 ; buffer
          48 ba 0d 00 00 00 00 00 00 00                mov rdx, 13                  ; length
          0f 05                                        syscall

          48 83 c4 10                                  add rsp, 0x10
          48 b8 3c 00 00 00 00 00 00 00                mov rax, 60                  ; sys_exit
          48 bf 00 00 00 00 00 00 00 00                mov rdi, 0                   ; status
          0f 05                                        syscall

; total 130 bytes machine code
```

### Reference timings (`compile_pipeline`, 9 cases)

Each table is the **sum of per-case mean times** in milliseconds (compile time rises with optimization; run time usually drops). Recorded on an Intel Core i9-14900K; treat as indicative only.

**Linux (`x86_64-linux`):**

```text
=== TOTALS (sum of per-case mean times over 9 cases) ===
lvl      sum_compile_mean_ms        sum_run_mean_ms
O0                  3429.332                918.855
O1                  3484.395                 88.178
O2                  3799.838                 87.838
O3                  3809.176                 85.724
ALL_O              14522.741               1180.596
```

**Windows (`x86_64-windows`):**

```text
=== TOTALS (sum of per-case mean times over 9 cases) ===
lvl      sum_compile_mean_ms        sum_run_mean_ms
O0                  4412.682               1151.832
O1                  4440.346                223.700
O2                  4789.201                193.640
O3                  4770.761                194.311
ALL_O              18412.990               1763.483
```

Approximate reproduction:

```bash
cargo test --test compile_pipeline -- --ignored --nocapture
```

---

Development workflow and conventions are described in `CONTRIBUTING.md` ([简体中文](./docs/CONTRIBUTING_CN.md)).

CI runs:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
