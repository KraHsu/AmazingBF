# AmazingBF 参赛说明

参赛人：`KraHsu`

本仓库是“十行代码”挑战赛参赛作品，对应两个赛道：

- 赛道一：Brainfuck 解释器 / 编译器
- 赛道二：弹幕游戏

项目主体使用 Rust 编写，提供 4 个可执行入口：

- `AmazingBF`：完整命令行入口
- `bf-interpreter`：固定为解释执行
- `bf-compiler`：固定为编译为原生可执行文件
- `bfsc`：将 BFS（Brainf Script）编译为 Brainfuck

图形界面作品通过 `bf-gui` 运行，它读取 `.bf` 或 `.bfs` 文件并驱动 Tauri GUI。

## 运行环境

- Rust stable（见 `rust-toolchain.toml`）
- Linux 或 Windows
- 创意赛道 GUI 额外需要：
  - `cargo` 可构建 `tauri` feature
  - Linux 下需要可用的 WebKitGTK / 桌面图形环境

## 构建

```bash
cargo build --release
```

如需构建 GUI：

```bash
cargo build --release --features gui --bin bf-gui
```

## 赛道一：Brainfuck 解释器 / 编译器

- [解释器文档](docs/interpreter.md)
- [编译器文档](docs/compiler.md)

### 解释器入口

解释器接收 Brainfuck 文件路径，并从标准输入读取程序输入。

```bash
./target/release/bf-interpreter tests/cases/1.bf < tests/cases/1.in
```

也可以使用完整入口：

```bash
./target/release/AmazingBF tests/cases/1.bf -q < tests/cases/1.in
```

### 编译器入口

编译器接收 Brainfuck 文件路径，并生成目标可执行文件。

```bash
./target/release/bf-compiler tests/cases/1.bf -o hello_bf
./hello_bf < tests/cases/1.in
```

交叉输出 Windows 可执行文件：

```bash
./target/release/bf-compiler tests/cases/1.bf --target x86_64-windows -o hello_bf.exe
```

### BFS 与自举解释器

仓库同时保留了 BFS 编译器 `bfsc`，以及使用 BFS 编写的自举解释器`examples/bf_self_host.bfs`。（这怎么不算解释器呢

## 赛道二：弹幕游戏

创意赛道作品说明见：

- [弹幕游戏文档](docs/gui_danmaku.md)

核心脚本是 [`examples/gui_danmaku.bfs`](examples/gui_danmaku.bfs)，玩法为
在 256x256 像素画面中躲避弹幕，坚持完整个六段波次。

构建并运行示例：

```bash
./target/release/bfsc examples/gui_danmaku.bfs -o /tmp/gui_danmaku.bf
./target/release/bf-gui /tmp/gui_danmaku.bf
```

也可以直接让 `bf-gui` 读取 `.bfs` 源文件：

```bash
./target/release/bf-gui examples/gui_danmaku.bfs
```

## 功能简介

- 支持标准 Brainfuck 八指令 `><+-.,[]`
- 支持解释执行与原生编译
- 编译目标支持 `x86_64-linux` 与 `x86_64-windows`
- BFS 提供变量、数组、条件、循环、函数与简单图形接口
- GUI 运行时支持键盘输入与 256x256 RGB332 帧缓冲绘制

## 开源库与外部工具说明

- Rust 标准工具链
- `tauri`：用于 GUI 窗口、IPC 与前端承载

复杂赛题说明：本项目的 `bf-compiler` 直接生成目标平台可执行文件（Linux ELF / Windows PE64）。（所以显然支持 **交叉编译！**

## AI 使用说明

- 使用工具：OpenAI Codex + Claude Code （从 8f913772d0aeb63778774008bbbf5567eeacbde8 开始，此前完全为手写，此后大量使用 AI，最开始的 commit 会通过 [manual/cursor] 来区分 ai 和 人类，但后来我放弃了，AI参与太深，**分不清，我真的分不清！**
- AI 辅助部分：
  - 参赛文档结构整理
  - 仓库冗余内容清理
  - 发布流程执行辅助
  - 几乎全部 windows PE64
  - 大部分 编译优化
- 自主完成部分：
  - Brainfuck 解释器、编译器、BFS 编译器与 GUI 运行链路的设计与实现
  - 弹幕游戏脚本与自举解释器脚本本体

## 仓库内容

- `src/`：解释器、编译器、BFS 编译器、GUI 运行时源码
- `examples/`：弹幕游戏与 BFS 自举解释器示例
- `tests/`：解释器、编译器、BFS、自举相关测试
- `docs/`：本次参赛说明文档
