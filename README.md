# AmazingBF [简体中文](./docs/README_CN.md)

`AmazingBF` is a Brainfuck toolchain project written in Rust. It currently provides two executable paths:

* **Interpretation**: parses the source code and runs it on HIR
* **Native compilation**: compiles LIR into x86_64 native executables (Linux ELF and Windows PE64 backends implemented)

Running `cargo build` produces three entry points:

* `AmazingBF` (full CLI with `-m` / `--mode` / `--target`)
* `bf-interpreter` (fixed interpretation mode)
* `bf-compiler` (fixed compilation mode, default target follows build target, also supports cross-compilation via `--target`)

The latter two are **specifically designed for ten-line code benchmarks**, and behave the same as `AmazingBF -m interpret -q` and `AmazingBF -m compile`.

---

## Current Capabilities

* Supports basic Brainfuck syntax: `><+-.,[]`
* Provides three execution modes: `interpret`, `compile`, and `dump`
* Complete frontend pipeline: `Lexer -> Parser -> AST`
* Layered intermediate representations: `HIR -> optimize -> LIR`
* Interpreter executes based on `HIR`
* Native backend generates `Linux ELF` or `Windows PE64`, and outputs `.asm` / `.lst` debug files with the same basename as the target
* Structured logging via `tracing`, with support for `RUST_LOG` overrides and JSON output

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

A man page source is included:
`man/amazingbf.1` → preview with `man -l man/amazingbf.1`

---

### Logging

* Default: human-readable terminal output
* `-v/-vv/-vvv`: increase verbosity
* `-q`: silence logs

Environment variables:

* `RUST_LOG`: override filters
* `AMAZINGBF_LOG_FORMAT=json`: enable JSON logging
* `AMAZINGBF_LOG_JSON=1`: compatibility switch

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
  cli.rs
  app.rs
  driver/
  frontend/
  ir/
  interp/
  runtime/
  backend/
tests/
  cases/
  *.rs
```

---

### Key Modules

* `main.rs`: default entry → calls `AmazingBF::run_amazingbf()`
* `app.rs`: CLI parsing, logging init, driver dispatch
* `cli.rs`: converts CLI args into `DriverConfig`
* `driver/logging.rs`: tracing setup and JSON switching
* `driver/run.rs`: mode dispatch and output handling
* `interp/engine.rs`: HIR interpreter
* `backend/codegen.rs`: LIR → assembly IR
* `backend/x86_64/`: machine code + ELF/PE generation

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
* `windows_target.rs`: PE64 structure and cross-compilation
* `compile_pipeline.rs`: slower compile benchmarks (`#[ignore]`)

---

Development workflow and conventions are described in `CONTRIBUTING.md`.
CI runs:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
