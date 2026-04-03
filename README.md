# AmazingBF

`AmazingBF` 是一个用 Rust 编写的 Brainfuck 工具链项目，目前包含两条可运行路径：

- 解释执行：将源码解析后在 HIR 上运行
- 原生编译：将 LIR 编译为 Linux x86_64 ELF 可执行文件

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
```

默认模式是 `interpret`，上面的样例会输出经典的 Hello World。

在解释模式下可加 `--interp-debug`，程序运行结束后在 **stderr** 打印 tape 统计：初始/最终槽位数、指针访问过的下标区间宽度、因右移而自动扩容的字节数，以及指针左移/右移的累计步数（与 HIR `Move` 合在一起的位移总量）。

### 编译为可执行文件

```bash
cargo run -- tests/cases/1.bf -m compile -o hello_bf
./hello_bf
```

执行编译模式时，程序会额外在输出文件旁边生成调试产物：

- `hello_bf.asm`：可读的汇编 listing
- `hello_bf.lst`：带偏移的十六进制 listing

调试产物路径使用 Rust `Path::with_extension()` 规则：

- `-o hello_bf` 会生成 `hello_bf.asm` / `hello_bf.lst`
- 默认输出 `-o a.out` 会生成 `a.asm` / `a.lst`

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
  main.rs              # 程序入口，初始化日志并启动 driver
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

## 已知问题

- `compile` 模式面向 Linux x86_64 后端
- `dump` 模式目前会跑到 `AsmProgram`，但不会额外导出 HIR/LIR/ASM 可视化产物
- 当前原生编译链路是手写的 `x86_64 ELF` 后端，**不再依赖 LLVM**
- `src/backend/` 下有较完整的中文模块文档，适合继续作为后端开发入口
- `compile` 产物的 ELF 代码段默认按只读可执行（RX）封装，运行时 tape 由匿名 `mmap` 单独提供

## 适合继续演进的方向

- 为 `cargo test` 补上真正的 CLI / IR / backend 测试
- 让 `dump` 模式输出 HIR、LIR 或汇编中间结果
- 在现有 LIR 基础上继续添加 peephole 或 loop 优化

