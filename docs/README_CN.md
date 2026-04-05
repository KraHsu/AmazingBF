# AmazingBF

`AmazingBF` 是一个用 Rust 编写的 Brainfuck 工具链项目，目前包含两条可运行路径：

- 解释执行：将源码解析后在 HIR 上运行
- 原生编译：将 LIR 编译为 x86_64 原生可执行文件（已实现 Linux ELF 与 Windows PE64 后端）

`cargo build` 会生成三个入口：`AmazingBF`（完整 CLI，含 `-m` / `--mode` / `--target`）、`bf-interpreter`（固定解释模式）、`bf-compiler`（固定编译模式，默认 target 跟随构建目标，也支持 `--target` 交叉编译）。后两者**专为十行代码测评提供**，与 `AmazingBF -m interpret -q` / `AmazingBF -m compile` 行为一致。

## 当前能力

- 支持 Brainfuck 基本语法：`><+-.,[]`
- 提供 `interpret`、`compile`、`dump` 三种运行模式
- 前端链路完整：`Lexer -> Parser -> AST`
- 中间表示分层：`HIR -> optimize -> LIR`
- 解释器基于 `HIR` 执行
- 原生后端可生成 `Linux ELF` 或 `Windows PE64`，并输出与目标路径同 `basename` 的 `.asm` / `.lst` 调试文件
- 提供基于 `tracing` 的结构化日志，支持 `RUST_LOG` 覆盖和 JSON 输出

## 快速开始

### 环境要求

- `Rust stable`
- 若使用 `AmazingBF` 的 `compile` 模式或 `bf-compiler`，可用 `--target x86_64-linux|x86_64-windows` 选择目标；**默认跟随构建目标**。
- 当前实际落地的原生后端包括 `x86_64-linux`（ELF）与 `x86_64-windows`（PE64）

### 构建

```bash
cargo build --release
```

### 解释执行

```bash
cargo run --bin AmazingBF -- tests/cases/1.bf -q
# 或特化入口
cargo run --bin bf-interpreter -- tests/cases/1.bf
```

`AmazingBF` 默认模式是 `interpret`，上面的样例会输出经典的 Hello World。

在解释模式下可加 `--interp-debug`，程序运行结束后在 **stderr** 打印 tape 统计：初始/最终槽位数、指针访问过的下标区间宽度、因右移而自动扩容的字节数，以及指针左移/右移的累计步数。

### 编译为可执行文件

```bash
cargo run --bin AmazingBF -- tests/cases/1.bf -m compile --target x86_64-linux -o hello_bf
# 或
cargo run --bin bf-compiler -- tests/cases/1.bf -o hello_bf
./hello_bf
```

`bf-compiler` 默认输出当前构建目标对应的格式，也可以显式交叉编译：

```bash
cargo run --bin bf-compiler -- tests/cases/1.bf --target x86_64-windows -o hello_bf
```
**当目标是 windows 时，会默认使用 `.exe` 后缀**

执行编译模式时，程序会额外在输出文件旁边生成调试产物：

- `hello_bf.asm`：可读的汇编 listing
- `hello_bf.lst`：带偏移的十六进制 listing

调试产物路径使用 Rust `Path::with_extension()` 规则：

- `-o hello_bf` 会生成 `hello_bf.asm` / `hello_bf.lst`
- Linux 目标默认输出 `a.out`，会生成 `a.asm` / `a.lst`
- Windows 目标默认输出 `a.exe`，会生成 `a.asm` / `a.lst`

编译优化级别由 `-O` / `--opt-level` 指定（默认 `0`）。当前实现中：

- `-O0``：HIR` 上仅合并连续的 `Move` / `Add`（单次扫描）；随后走常规 `LIR` → 汇编路径。
- `-O1`：在 `-O0` 基础上再做**一次** HIR 扫描：将 `[-]` 化为 `Zero`；将仅含单方向移动的循环 `[>]` / `[<]` 化为 `Scan`；将仅含加减与指针回退的仿射循环（如 `[->+<]`、`[->>++<<]`）化为 `LinearMul`；在 tape 全零初值假设下做简单常数传播，并删除入口格为 0 的空循环体 `[]`。窥孔化简包含 `Add; Zero`→`Zero` 等等价情形；**不会**把 `Zero; Add(k)` 合成单次 `Add(k)`（前者为先清零再累加，后者为在旧值上相对加）。
- `-O2`：在 `-O1` 基础上**重复**整条 `HIR` 优化管线，直到程序到达不动点。
- `-O3`：`HIR` 与 `-O2` 相同；并在 **compile** 模式下额外启用最强编译期折叠：若源码中没有任何 `.`，生成仅 `exit(0)` 的极小目标平台可执行文件；若有 `.` 且没有任何 `,`，则在编译时于 `HIR` 上解释执行一次以收集标准输出字节序列，再生成直接输出该序列后退出的目标平台可执行文件（`Linux` 走 `write+exit`，`Windows` 走 `WriteFile+ExitProcess`）。

比如对于 `tests/cases/1.bf` 将会直接输出：

```hex
; === Brainfuck x86_64 Hex Listing ===
; 共 1 条指令，编码后 130 字节

Offset    Hex                                          Assembly
--------- -------------------------------------------- ----------------------------------------
0x0000:   48 83 ec 10                                  sub rsp, 0x10                ; 栈上分配 16 字节缓冲区
          c6 44 24 00 48                               mov byte ptr [rsp+0x00],0x48 ; buf[0]  = 'H'
          c6 44 24 01 65                               mov byte ptr [rsp+0x01],0x65 ; buf[1]  = 'e'
          c6 44 24 02 6c                               mov byte ptr [rsp+0x02],0x6c ; buf[2]  = 'l'
          c6 44 24 03 6c                               mov byte ptr [rsp+0x03],0x6c ; buf[3]  = 'l'
          c6 44 24 04 6f                               mov byte ptr [rsp+0x04],0x6f ; buf[4]  = 'o'
          c6 44 24 05 20                               mov byte ptr [rsp+0x05],0x20 ; buf[5]  = ' '
          c6 44 24 06 57                               mov byte ptr [rsp+0x06],0x57 ; buf[6]  = 'W'
          c6 44 24 07 6f                               mov byte ptr [rsp+0x07],0x6f ; buf[7]  = 'o'
          c6 44 24 08 72                               mov byte ptr [rsp+0x08],0x72 ; buf[8]  = 'r'
          c6 44 24 09 6c                               mov byte ptr [rsp+0x09],0x6c ; buf[9]  = 'l'
          c6 44 24 0a 64                               mov byte ptr [rsp+0x0a],0x64 ; buf[10] = 'd'
          c6 44 24 0b 21                               mov byte ptr [rsp+0x0b],0x21 ; buf[11] = '!'
          c6 44 24 0c 0a                               mov byte ptr [rsp+0x0c],0x0a ; buf[12] = '\n'

          48 b8 01 00 00 00 00 00 00 00                mov rax, 1                   ; rax = 1, Linux x86_64 sys_write
          48 bf 01 00 00 00 00 00 00 00                mov rdi, 1                   ; rdi = 1, fd = stdout
          48 89 e6                                     mov rsi, rsp                 ; rsi = buf 指针（栈顶）
          48 ba 0d 00 00 00 00 00 00 00                mov rdx, 13                  ; rdx = 长度 13
          0f 05                                        syscall                      ; write(1, rsp, 13)

          48 83 c4 10                                  add rsp, 0x10                ; 回收前面申请的 16 字节栈空间
          48 b8 3c 00 00 00 00 00 00 00                mov rax, 60                  ; rax = 60, Linux x86_64 sys_exit
          48 bf 00 00 00 00 00 00 00 00                mov rdi, 0                   ; rdi = 0, exit code
          0f 05                                        syscall                      ; exit(0)

; 总计 130 字节机器码
```

优化性能测试结果如下：
Linux:
```bash
=== TOTALS (sum of per-case mean times over 9 cases) ===
lvl      sum_compile_mean_ms        sum_run_mean_ms
O0                  3413.435                917.180
O1                  3461.116                 85.636
O2                  3785.449                 85.710
O3                  3798.939                 90.005
ALL_O              14458.939               1178.532
```
Windows:
```bash
=== TOTALS (sum of per-case mean times over 9 cases) ===
lvl      sum_compile_mean_ms        sum_run_mean_ms
O0                  4652.539               1403.737
O1                  4711.792                197.599
O2                  5098.319                202.331
O3                  5068.148                196.842
ALL_O              19530.798               2000.509
```

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
  -> target-specific executable
```

当前 `driver` 会统一执行到**优化后的 HIR**，然后再按模式分支：

- `interpret`：`source -> lexer -> parser -> AST -> HIR -> optimize`，随后直接在 `HIR` 上解释执行
- `compile`：`source -> lexer -> parser -> AST -> HIR -> optimize -> LIR -> AsmProgram -> x86_64 backend -> target-specific executable`
- `dump`：`source -> lexer -> parser -> AST -> HIR -> optimize -> LIR -> AsmProgram`，仅记录日志，不写出产物

### 模块分层

```text
src/
  lib.rs               # 共享模块树与 `run_*` 入口（供默认二进制与 `bf-*` 使用）
  main.rs              # `AmazingBF` 默认二进制入口
  bin/bf-interpreter.rs
  bin/bf-compiler.rs   # 固定模式的前端，调用 lib 中对应 `run_*`
  cli.rs               # Clap CLI，构造 AppConfig / DriverConfig
  app.rs               # 二进制入口复用的启动逻辑（解析 CLI、初始化日志、调用 driver）
  driver/              # 配置、前端流水线与运行模式分发
  frontend/            # lexer / parser / AST
  ir/                  # HIR、LIR、lower、optimize
  interp/              # HIR 解释器
  runtime/             # tape、IO、host 抽象
  backend/             # 汇编 IR、代码生成、x86_64 原生后端
tests/
  cases/               # Brainfuck 测试样例与输入输出
  *.rs                 # 集成测试（解释器、编译器、平台相关回归）
```

### 关键模块说明

- `src/main.rs`
  默认二进制入口，仅调用库入口 `AmazingBF::run_amazingbf()`
- `src/app.rs`
  负责衔接 CLI 解析、日志初始化与 `driver::run()` 调度
- `src/cli.rs`
  负责把命令行参数解析成 `DriverConfig`，并把帮助/版本/参数错误显式返回给启动层处理
- `src/driver/logging.rs`
  负责统一 tracing subscriber、默认过滤级别和 JSON 日志切换
- `src/driver/run.rs`
  负责按 mode 分发解释执行或落盘编译产物；前端阶段与产物写出逻辑拆分在 `driver/` 内部子模块
- `src/interp/engine.rs`
  在 HIR 上执行程序，依赖 `Tape`、`RuntimeIo`、`HostRuntime`
- `src/backend/codegen.rs`
  把 LIR 翻译为汇编 IR，并固定了寄存器角色与 tape 扩容策略
- `src/backend/x86_64/`
  将汇编 IR 编码为机器码，并封装为 Linux ELF 或 Windows PE64 可执行文件

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

当前仓库可以正常运行 `cargo test`，并覆盖后端编码、ELF/PE 结构以及 `.lst` 与真实机器码的一致性等关键回归。

### 回归测试说明

```bash
cargo test
```

测试数据位于 `tests/cases/`，每个样例通常包含：

- `.bf`：Brainfuck 程序
- `.in`：输入（可选；缺失时按空输入处理）
- `.out`：预期输出
- `.md`：样例说明

现有测试大致分为三类：

- `tests/cases_pipeline.rs`：以 `tests/cases/*.bf` 为样例集，校验解释模式与编译模式输出
- `tests/windows_target.rs`：Windows PE64 目标结构、导入表和交叉编译回归
- `tests/compile_pipeline.rs`：较慢的编译模式统计与长耗时回归，默认 `#[ignore]`

开发流程和代码规范约定见 `CONTRIBUTING.md`。CI 会执行 `cargo fmt --check`、`cargo clippy --all-targets -- -D warnings` 和 `cargo test`。

