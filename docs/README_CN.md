# AmazingBF [English](../README.md)

`AmazingBF` 是一个用 Rust 编写的 Brainfuck 工具链项目，目前提供两条可执行路径：

* **解释执行**：解析源码后在 HIR 上运行
* **原生编译**：将 LIR 编译为 x86_64 原生可执行文件（已实现 Linux ELF 与 Windows PE64 后端）

此外还包含 `bfsc`，一个 **BFS（Brainf Script）** 领域特定语言编译器，可将 `.bfs` 源文件转译为 Brainfuck。

`cargo build` 会生成四个入口：

* `AmazingBF`（完整 CLI，含 `-m` / `--mode` / `--target`）
* `bf-interpreter`（固定解释模式）
* `bf-compiler`（固定编译模式，默认 target 跟随构建目标，也支持 `--target` 交叉编译）
* `bfsc`（BFS → BF 编译器）

第二、三个入口**专为十行代码测评设计**，行为分别与 `AmazingBF -m interpret -q` 和 `AmazingBF -m compile` 一致。

---

## 当前能力

* 支持 Brainfuck 基本语法：`><+-.,[]`
* 提供 `interpret`、`compile`、`dump` 三种运行模式
* 前端链路完整：`Lexer -> Parser -> AST`
* 中间表示分层：`HIR -> optimize -> LIR`
* 解释器基于 `HIR` 执行
* 原生后端可生成 `Linux ELF` 或 `Windows PE64`，并输出与目标路径同 `basename` 的 `.asm` / `.lst` 调试文件
* 流水线日志仅通过 `-q` / `-v` 控制，向 stderr 输出纯文本（无 `tracing` 等额外日志 crate）

---

## 快速开始

### 环境要求

* `Rust stable`
* 使用 `AmazingBF` 的 `compile` 模式或 `bf-compiler` 时，可用 `--target x86_64-linux|x86_64-windows` 选择目标；**默认跟随构建目标**
* 当前支持的原生后端：`x86_64-linux`（ELF）与 `x86_64-windows`（PE64）

---

### 构建

```bash
cargo build --release
```

`Cargo.toml` 中的 `release` 配置偏向缩小体积（`opt-level = "z"`、LTO、`strip`、`panic = "abort"`）。运行时除标准库外无额外依赖。

---

### 解释执行

```bash
cargo run --bin AmazingBF -- tests/cases/1.bf -q
# 或特化入口
cargo run --bin bf-interpreter -- tests/cases/1.bf
```

`AmazingBF` 默认模式为 `interpret`，上述示例会输出经典的 Hello World。

在解释模式下可加 `--interp-debug`，程序运行结束后在 **stderr** 打印 tape 统计：初始/最终槽位数、指针访问过的下标区间宽度、因右移扩容而分配的字节数，以及指针左移/右移的累计步数。

---

### 编译为可执行文件

```bash
cargo run --bin AmazingBF -- tests/cases/1.bf -m compile --target x86_64-linux -o hello_bf
# 或
cargo run --bin bf-compiler -- tests/cases/1.bf -o hello_bf
./hello_bf
```

`bf-compiler` 默认输出当前构建目标对应的格式，也支持显式交叉编译：

```bash
cargo run --bin bf-compiler -- tests/cases/1.bf --target x86_64-windows -o hello_bf
```

**当目标是 Windows 时，默认会追加 `.exe` 后缀。**

编译过程中会在输出文件旁生成额外调试产物：

* `hello_bf.asm`：可读的汇编 listing
* `hello_bf.lst`：带偏移的十六进制 listing

调试产物路径遵循 Rust 的 `Path::with_extension()` 规则：

* `-o hello_bf` → `hello_bf.asm` / `hello_bf.lst`
* Linux 默认输出 `a.out` → `a.asm` / `a.lst`
* Windows 默认输出 `a.exe` → `a.asm` / `a.lst`

---

### BFS（Brainf Script）编译器

`bfsc` 将 `.bfs` 源文件编译为 Brainfuck 文本，再交由 `AmazingBF` 解释或编译执行。

**支持的类型：**

| 类型  | BF 格数 | 表示方式                          |
|-------|---------|----------------------------------|
| `u8`  | 1       | 模 256 循环                      |
| `u16` | 2       | 小端序（低字节在前）              |
| `u32` | 4       | 小端序（低字节在前）              |

**语言特性：** 变量声明、数组、`while`、`if/else`、算术（`+ - * / %`）、比较（`< > <= >= == !=`）、布尔运算（`&& ||`）、`scan()`、`print()`、`putchar()`。

```bfs
// 示例：冒泡排序
let n: u8 = 0;
let arr: [u8; 10];
let i: u8 = 0;

scan(n);
i = 0;
while i < n { scan(arr[i]); i = i + 1; }
// ... 排序主体 ...
```

**使用方法：**

```bash
# 将 .bfs 编译为 BF 文本（输出到 stdout）
bfsc tests/utils/sort.bfs

# 保存 BF 文本到文件，再运行
bfsc tests/utils/sort.bfs -o /tmp/sort.bf
printf "3\n2\n1\n" | AmazingBF /tmp/sort.bf -q

# 将 .bfs 直接编译为原生可执行文件（无需生成中间 .bf 文件）
bfsc tests/utils/sort.bfs -c -o sort

# 指定目标平台
bfsc tests/utils/sort.bfs -c --target x86_64-linux -o sort

# 若 BF 程序不读取 stdin，可直接管道
bfsc tests/utils/linear_eq.bfs | AmazingBF - -q
```

**`bfsc` CLI 选项：**

| 选项 | 说明 |
|------|------|
| `-o, --output <PATH>` | 不加 `-c`：将 BF 文本写入文件（默认 stdout）；加 `-c`：可执行文件输出路径（默认 `a.out` / `a.exe`）。 |
| `-c, --compile` | 将 .bfs 直接编译为原生 x86_64 可执行文件（通过 AmazingBF 后端）。不加此选项时只输出 BF 文本。 |
| `--target <T>` | 编译目标（仅 `-c`）：`x86_64-linux` \| `x86_64-windows`，默认跟随构建目标。 |
| `-O, --opt-level <0-3>` | 传给 AmazingBF 后端的优化级别（仅 `-c`，默认 `3`）。 |
| `-q, --quiet` | 静默后端进度日志（仅 `-c`）。 |
| `-h, --help` | 显示帮助。 |
| `-V, --version` | 显示版本。 |

> **注意：** 当 BF 程序需要读取 stdin（例如含 `scan`）时，必须先将编译结果写入文件再以参数形式传给 `AmazingBF`。通过管道同时传入 BF 代码和程序输入不可行。

`bfsc` 的测试固件以 `.bfs` / `.in` / `.out` 三件套形式存放在 `tests/utils/`。

---

### 优化级别

通过 `-O` / `--opt-level` 设置（默认 `0`）：

* **O0**：在 HIR 上合并连续的 `Move` / `Add`（单次扫描），随后照常进入 LIR → 汇编

* **O1**：增加一轮 HIR 扫描：

  * `[-]` → `Zero`
  * `[>]` / `[<]` → `Scan`
  * 仿射循环（如 `[->+<]`、`[->>++<<]`）→ `LinearMul`
  * 在 tape 全零初值假设下的简单常数传播
  * 当入口格为 0 时删除空循环 `[]`
  * 窥孔化简，如 `Add; Zero → Zero`
  * **不会**将 `Zero; Add(k)` 折叠为 `Add(k)`（语义不同）

* **O2**：重复整条 HIR 优化管线直至达到不动点

* **O3**：HIR 与 O2 相同，并启用更强的编译期折叠：

  * 无 `.` → 生成仅 `exit(0)` 的极小可执行文件
  * 有 `.` 且无 `,` → 编译时在 HIR 上解释一次收集输出，再生成直接打印该序列后退出的程序（Linux 为 `write+exit`，Windows 为 `WriteFile+ExitProcess`）

---

### 查看 CLI 帮助

```bash
cargo run -- --help
# 仅摘要
cargo run -- -h
```

仓库内提供手册页源文件：
`man/amazingbf.1` 可本地预览：`man -l man/amazingbf.1`
`man/bfsc.1` 可本地预览：`man -l man/bfsc.1`

---

### 日志

流水线相关消息输出到 **stderr**，纯文本：

* 默认：少量进度行（如开始/结束、编译摘要）
* `-v` / `-vv` / `-vvv`：更详细的诊断
* `-q`：关闭上述消息（被解释执行的 Brainfuck 程序自身的 stdin/stdout 不受影响）

不支持 `RUST_LOG` 等环境变量覆盖。

---

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

BFS source (.bfs)
  -> bfsc（lexer -> parser -> typeck -> layout -> codegen）
  -> Brainfuck 文本
  -> AmazingBF（解释执行或编译）
```

Driver 行为：

* **interpret**：直接在优化后的 HIR 上运行
* **compile**：继续经 LIR → 后端 → 可执行文件
* **dump**：停在汇编 IR，仅记录日志

---

### 模块分层

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
  bfsc/          （lexer、parser、typeck、layout、codegen）
tests/
  cases/         （AmazingBF 的 BF 固件）
  utils/         （bfsc 的 BFS 固件）
  *.rs
```

---

### 关键模块

* `main.rs`：默认入口 → 调用 `AmazingBF::run_amazingbf()`
* `app.rs`：CLI 解析、日志初始化、driver 分发
* `cli.rs`：精简 argv 解析（无 `clap`）；将标志转为 `DriverConfig`
* `logging.rs`：由 CLI 设置的详细级别；`log_info` / `log_debug` 使用 `eprintln!`
* `driver/run.rs`：按模式分发与输出处理
* `interp/engine.rs`：HIR 解释器
* `backend/codegen.rs`：LIR → 汇编 IR
* `backend/x86_64/`：机器码与 ELF/PE 生成
* `bfsc/`：BFS 编译器，含 `lexer`、`parser`、`typeck`、`layout`、`codegen` 子模块

---

## 解释器与编译器的边界

* 解释器操作对象为 `HIR`
* 后端消费 `LIR`
* `AsmProgram` 为后端内部 IR
* 寄存器约定与内存策略在后端中定义

---

## 测试

### 运行测试

```bash
cargo test
```

覆盖后端编码、ELF/PE 结构以及 `.lst` 一致性。

---

### 测试结构

位于 `tests/cases/`，每个用例可包含：

* `.bf`：程序
* `.in`：输入（可选）
* `.out`：预期输出
* `.md`：说明

测试分类：

* `cases_pipeline.rs`：解释器与编译器输出对照
* `bfsc_pipeline.rs`：BFS 编译器端到端测试——`.bfs` → `bfsc` → `AmazingBF` → 与 `.out` 比对
* `windows_target.rs`：PE64 结构与交叉编译
* `compile_artifacts.rs`：O0–O3 下 ELF/PE 产物的合法性、`.asm`/`.lst` 输出以及 EOF 语义

---

## 基准测试

`benches/` 下提供两套基准，均为开发者本地使用（CI 仅执行 `cargo test`）。

### `compile_levels`（编译 + 运行耗时，9 个用例）

自定义 `main` 入口（不使用 Criterion）。针对 `tests/cases/*.bf` × `-O0..3` 的每个组合做 `TRIALS = 10` 轮「先编译、后运行」循环，打印每个用例的 compile ms / run ms 均值/最小/最大，并在末尾给出每个优化级别下「每个用例均值之和」的总表。在 Unix 上，每组合的首轮还会通过 `/usr/bin/time` 采集 max RSS。**不再重复校验** ELF/PE / `.asm` / `.lst` / EOF 正确性——该部分已交由 `tests/compile_artifacts.rs` 负责。

运行：

```bash
cargo bench --bench compile_levels
```

参考总表——**每个用例平均耗时的总和**（毫秒）：编译时间通常随级别上升，运行时间往往下降。数据在 Intel Core i9-14900K 上采集，仅供参考。

**Linux（`x86_64-linux`）：**

```text
=== TOTALS (sum of per-case mean times over 9 cases) ===
lvl      sum_compile_mean_ms        sum_run_mean_ms
O0                  3429.332                918.855
O1                  3484.395                 88.178
O2                  3799.838                 87.838
O3                  3809.176                 85.724
ALL_O              14522.741               1180.596
```

**Windows（`x86_64-windows`）：**

```text
=== TOTALS (sum of per-case mean times over 9 cases) ===
lvl      sum_compile_mean_ms        sum_run_mean_ms
O0                  4412.682               1151.832
O1                  4440.346                223.700
O2                  4789.201                193.640
O3                  4770.761                194.311
ALL_O              18412.990               1763.483
```

### `standard_suite`（跨优化级别的运行时基准，matslina BF 程序）

基于 Criterion 的基准，覆盖 `long`、`dbfi`、`factor`、`mandelbrot`、`hanoi` 等 matslina 经典 BF 程序在可行的 O0–O3 下解释与原生编译执行两条路径。

```bash
cargo bench --bench standard_suite
```

Criterion 基线对比（修改前先保存基线，修改后再对比）：

```bash
cargo bench --bench standard_suite -- --save-baseline main
# ... 修改代码 ...
cargo bench --bench standard_suite -- --baseline main
```

---

## 附录

### 示例 `.lst` 片段（`tests/cases/1.bf`，`-O3`，`x86_64-linux`）

在 **O3** 下，含 `.` 且无 `,` 的程序可能被折叠为极小的 `write` + `exit` 可执行文件。下面是生成 `.lst` 中的代表性片段（具体排版可能随版本略有差异）。

```text
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

---

开发流程与约定见根目录 `CONTRIBUTING.md`（中文版：`docs/CONTRIBUTING_CN.md`）。CI 执行：

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
