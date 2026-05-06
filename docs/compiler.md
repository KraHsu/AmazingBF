# 赛道一子文档：Brainfuck 编译器

## 作品定位

本项目提供 `bf-compiler`，用于完成比赛“复杂赛题（Brainfuck 编译器）”。
接收 Brainfuck 文件路径，输出可直接运行的原生可执行文件。

对应二进制：

- `bf-compiler`
- `AmazingBF` 的编译模式

## 构建

```bash
cargo build --release
```

产物路径：

- `./target/release/bf-compiler`
- `./target/release/AmazingBF`

## 运行方式

按比赛评测方式生成程序：

```bash
./target/release/bf-compiler tests/cases/1.bf -o hello_bf
```

生成后执行：

```bash
./hello_bf < tests/cases/1.in > /tmp/out.txt
```

等价完整入口：

```bash
./target/release/AmazingBF tests/cases/1.bf -m compile -o hello_bf
```

## 输出目标

- `x86_64-linux`：生成 ELF 可执行文件
- `x86_64-windows`：生成 PE64 可执行文件

默认目标跟随构建当前二进制时的平台，也可以显式指定：

```bash
./target/release/bf-compiler tests/cases/1.bf --target x86_64-windows -o hello_bf.exe
```

## 产物说明

主输出之外，还会在同目录生成调试辅助文件：

- `.asm`
- `.lst`

例如：

```bash
./target/release/bf-compiler tests/cases/1.bf -o hello_bf
```

会同时生成：

- `hello_bf`
- `hello_bf.asm`
- `hello_bf.lst`

## 说明：NO 现成编译工具

本项目的 `bf-compiler` 不调用 `clang`、`gcc` 等现成 C/C++ 编译器；而是直接从内部
IR 生成目标平台可执行文件。

## 优化

支持 `-O0` 到 `-O3`。示例：

```bash
./target/release/bf-compiler tests/cases/1.bf -O3 -o hello_bf
```

优化主要作用于 HIR，包含：

- 连续移动与加减合并
- `[-]`、`[>]`、`[<]` 等模式识别
- 线性乘法循环识别
- 更高等级下的固定点优化与部分编译期折叠

## 测试

编译器行为主要由以下测试覆盖：

- `tests/cases_pipeline.rs`
- `tests/compile_artifacts.rs`
- `tests/windows_target.rs`
