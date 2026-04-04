# AmazingBF

`AmazingBF` 是一个用 Rust 编写的 Brainfuck 工具链项目，目前包含两条可运行路径：

- 解释执行：将源码解析后在 HIR 上运行
- 原生编译：将 LIR 编译为 Linux x86_64 ELF 可执行文件

`cargo build` 会生成三个入口：`AmazingBF`（完整 CLI，含 `-m` / `--mode`）、`bf-interpreter`（固定解释模式）、`bf-compiler`（固定编译模式）。后两者与 `AmazingBF -m interpret -q` / `AmazingBF -m compile` 行为一致，无需再传 `-m`。**此专为十行代码测评提供**

## 当前能力

- 支持 Brainfuck 基本语法：`><+-.,[]`
- 提供 `interpret`、`compile`、`dump` 三种运行模式
- 前端链路完整：`Lexer -> Parser -> AST`
- 中间表示分层：`HIR -> optimize -> LIR`
- 解释器基于 HIR 执行
- 原生后端可生成 ELF，并输出与目标路径同 basename 的 `.asm` / `.lst` 调试文件
- 提供基于 `tracing` 的结构化日志，支持 `RUST_LOG` 覆盖和 JSON 输出

## 快速开始

### 环境要求

- Rust stable
- 若使用 `compile` 模式，目标平台默认为 Linux x86_64

### 构建

```bash
cargo build
```

### 解释执行

```bash
cargo run -- tests/cases/1.bf -q
# 或特化入口
cargo run --bin bf-interpreter -- tests/cases/1.bf
```

默认模式是 `interpret`，上面的样例会输出经典的 Hello World。

在解释模式下可加 `--interp-debug`，程序运行结束后在 **stderr** 打印 tape 统计：初始/最终槽位数、指针访问过的下标区间宽度、因右移而自动扩容的字节数，以及指针左移/右移的累计步数。

### 编译为可执行文件

```bash
cargo run -- tests/cases/1.bf -m compile -o hello_bf
# 或
cargo run --bin bf-compiler -- tests/cases/1.bf -o hello_bf
./hello_bf
```

执行编译模式时，程序会额外在输出文件旁边生成调试产物：

- `hello_bf.asm`：可读的汇编 listing
- `hello_bf.lst`：带偏移的十六进制 listing

调试产物路径使用 Rust `Path::with_extension()` 规则：

- `-o hello_bf` 会生成 `hello_bf.asm` / `hello_bf.lst`
- 默认输出 `-o a.out` 会生成 `a.asm` / `a.lst`

编译优化级别由 `-O` / `--opt-level` 指定（默认 `0`）。当前实现中：

- `-O0`：HIR 上仅合并连续的 `Move` / `Add`（单次扫描）；随后走常规 LIR → 汇编路径。
- `-O1`：在 `-O0` 基础上再做**一次** HIR 扫描：将 `[-]` 化为 `Zero`；将仅含单方向移动的循环 `[>]` / `[<]` 化为 `Scan`；将仅含加减与指针回退的仿射循环（如 `[->+<]`、`[->>++<<]`）化为 `LinearMul`；在 tape 全零初值假设下做简单常数传播，并删除入口格为 0 的空循环体 `[]`。窥孔化简包含 `Add; Zero`→`Zero` 等等价情形；**不会**把 `Zero; Add(k)` 合成单次 `Add(k)`（前者为先清零再累加，后者为在旧值上相对加）。
- `-O2`：在 `-O1` 基础上**重复**整条 HIR 优化管线，直到程序到达不动点。
- `-O3`：HIR 与 `-O2` 相同；并在 **compile** 模式下额外启用最强编译期折叠：若源码中没有任何 `.`，生成仅 `exit(0)` 的极小 ELF；若有 `.` 且没有任何 `,`，则在编译时于 HIR 上解释执行一次以收集标准输出字节序列，再生成直接 `write` 该序列后 `exit` 的 ELF。

比如对于 `tests/cases/1.bf` 将会直接输出：

```hex
; === Brainfuck x86_64 Hex Listing ===
; 共 1 条指令，编码后 130 字节

Offset    Hex                                          Assembly
--------- -------------------------------------------- ----------------------------------------
0x0000:  48 83 ec 10 c6 44 24 00 48 c6 44 24 01 65    ; <raw 130 bytes: precomputed -O3 machine code>
         c6 44 24 02 6c c6 44 24 03 6c c6 44 24 04
         6f c6 44 24 05 20 c6 44 24 06 57 c6 44 24
         07 6f c6 44 24 08 72 c6 44 24 09 6c c6 44
         24 0a 64 c6 44 24 0b 21 c6 44 24 0c 0a 48
         b8 01 00 00 00 00 00 00 00 48 bf 01 00 00
         00 00 00 00 00 48 89 e6 48 ba 0d 00 00 00
         00 00 00 00 0f 05 48 83 c4 10 48 b8 3c 00
         00 00 00 00 00 00 48 bf 00 00 00 00 00 00
         00 00 0f 05

; 总计 130 字节机器码
```

优化性能测试结果如下：
```bash
=== TOTALS (sum of per-case mean times over 9 cases) ===
lvl      sum_compile_mean_ms        sum_run_mean_ms
O0                  3413.435                917.180
O1                  3461.116                 85.636
O2                  3785.449                 85.710
O3                  3798.939                 90.005
ALL_O              14458.939               1178.532
test compile_mode_emits_rx_elf_artifacts_and_preserves_eof_semantics ... ok
```

更完整的测试可以使用 `cargo test --test compile_pipeline -- --ignored --nocapture` 命令

### 查看 CLI 帮助

```bash
cargo run -- --help
# 仅摘要
cargo run -- -h
```

仓库内提供手册页源文件 `man/amazingbf.1`，可本地预览：`man -l man/amazingbf.1`（安装到系统手册目录后即可用 `man amazingbf`）。

### 日志

默认日志输出为适合终端阅读的文本格式，`-v/-vv/-vvv` 会提升默认详细度，`-q` 会把默认过滤级别降为静默。

- `RUST_LOG`：覆盖默认过滤规则，例如 `RUST_LOG=debug cargo run -- tests/cases/1.bf`
- `AMAZINGBF_LOG_FORMAT=json`：切换为 JSON 日志
- `AMAZINGBF_LOG_JSON=1`：JSON 日志的兼容开关

## 项目架构

### 总体流水线

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
  -> ELF
```

当前 `driver` 会统一执行到 `AsmProgram`，然后再按模式分支：

- `interpret`：`source -> lexer -> parser -> AST -> HIR -> optimize -> LIR -> AsmProgram`，最终仍在 `HIR` 上解释执行
- `compile`：`source -> lexer -> parser -> AST -> HIR -> optimize -> LIR -> AsmProgram -> x86_64 backend -> ELF`
- `dump`：`source -> lexer -> parser -> AST -> HIR -> optimize -> LIR -> AsmProgram`，仅记录日志，不写出产物

### 模块分层

```text
src/
  lib.rs               # 共享模块树与 `run_*` 入口（供默认二进制与 `bf-*` 使用）
  main.rs              # `AmazingBF` 默认二进制入口
  bin/bf-interpreter.rs
  bin/bf-compiler.rs   # 固定模式的前端，调用 lib 中对应 `run_*`
  cli.rs               # Clap CLI，构造 AppConfig / DriverConfig
  driver/              # 运行模式分发与整体流水线编排
  frontend/            # lexer / parser / AST
  ir/                  # HIR、LIR、lower、optimize
  interp/              # HIR 解释器
  runtime/             # tape、IO、host 抽象
  backend/             # 汇编 IR、代码生成、x86_64 ELF 后端
tests/
  cases/               # Brainfuck 测试样例与输入输出
  test.sh              # shell 测试脚本
```

### 关键模块说明

- `src/main.rs`
  负责解析 CLI、初始化 tracing 日志，并调用 `driver::run::run()`
- `src/cli.rs`
  负责把命令行参数解析成 `DriverConfig`
- `src/driver/logging.rs`
  负责统一 tracing subscriber、默认过滤级别和 JSON 日志切换
- `src/driver/run.rs`
  负责串起 `lex -> parse -> lower_to_hir -> optimize -> lower_to_lir -> compile_lir_to_asm`，再按 mode 分发解释执行或落盘编译产物
- `src/interp/engine.rs`
  在 HIR 上执行程序，依赖 `Tape`、`RuntimeIo`、`HostRuntime`
- `src/backend/codegen.rs`
  把 LIR 翻译为汇编 IR，并固定了寄存器角色与 tape 扩容策略
- `src/backend/x86_64/`
  将汇编 IR 编码为机器码，并封装为 ELF 可执行文件

## 解释器与编译器的边界

- 解释器执行的是 `HIR`
- 原生后端消费的是 `LIR`
- `AsmProgram` 是后端内部使用的汇编级 IR
- `src/backend/asm.rs` 与 `src/backend/codegen.rs` 一起定义了寄存器约定、标签分配和扩容策略

这意味着如果要贡献：

- 语法和语言前端：优先看 `src/frontend/`
- 优化和 IR 结构：优先看 `src/ir/`
- 运行语义：优先看 `src/interp/` 与 `src/runtime/`
- 原生代码生成：优先看 `src/backend/`

## 测试

### Rust 测试

```bash
cargo test
```

当前仓库可以正常运行 `cargo test`，并覆盖后端编码、ELF 结构以及 `.lst` 与真实机器码一致性等关键回归；端到端样例仍主要通过独立 bash 脚本完成。

### Shell 样例测试

```bash
cargo build --release
cargo test --all
```

测试数据位于 `tests/cases/`，每个样例通常包含：

- `.bf`：Brainfuck 程序
- `.in`：输入（可选；缺失时按空输入处理）
- `.out`：预期输出
- `.md`：样例说明

