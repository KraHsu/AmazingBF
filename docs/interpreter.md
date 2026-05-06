# 赛道一子文档：Brainfuck 解释器

## 作品定位

本项目提供 `bf-interpreter`，用于完成比赛“简单赛题（Brainfuck 解释器）”。
它接收 Brainfuck 文件路径，读取标准输入作为 `,` 指令输入，并将程序输出写到标准输出。

对应二进制：

- `bf-interpreter`
- `AmazingBF` 的解释模式

## 构建

```bash
cargo build --release
```

产物路径：

- `./target/release/bf-interpreter`
- `./target/release/AmazingBF`

## 运行方式

按比赛评测方式运行：

```bash
./target/release/bf-interpreter tests/cases/1.bf < tests/cases/1.in > /tmp/out.txt
```

等价完整入口：

```bash
./target/release/AmazingBF tests/cases/1.bf -q < tests/cases/1.in > /tmp/out.txt
```

如果 Brainfuck 源程序本身从标准输入提供，可以使用 `-`：

```bash
cat tests/cases/1.bf | ./target/release/bf-interpreter -
```

## 行为说明

- 支持 Brainfuck 八指令：`><+-.,[]`
- 默认先完成前端解析与 HIR 优化，再在解释器中执行
- `-q` 可关闭进度日志
- `--interp-debug` 可在 stderr 输出 tape 使用统计

## 样例

```bash
./target/release/bf-interpreter tests/cases/1.bf < tests/cases/1.in
```

`tests/cases/` 目录中提供了对应的 `.bf` / `.in` / `.out` 样例。

## BFS 自举解释器

仓库还包含一个用 BFS 写成的 Brainfuck 解释器：

- [`examples/bf_self_host.bfs`](../examples/bf_self_host.bfs)

它不是 `bf-interpreter` 主入口的一部分，但属于本次参赛作品展示内容：先用
`bfsc` 把 BFS 编译成 Brainfuck，再交给 `AmazingBF` 运行，形成“BF 工具链运行
BF 解释器”的自举效果。

### 编译自举解释器

```bash
./target/release/bfsc examples/bf_self_host.bfs -o /tmp/bf_self_host.bf
```

### 运行方式

自举解释器的标准输入格式为：

```text
<bf_program>!<bf_program_input>
```

其中 `!` 用作被解释程序与其输入的分隔符。

示例：

```bash
printf '%s!' "$(tr -d '\n' < tests/cases/1.bf)" \
  | ./target/release/AmazingBF /tmp/bf_self_host.bf -q
```

## 限制说明

- 主解释器没有参赛题面以外的额外输入格式要求
- 自举解释器受其脚本内部数组预算限制，只适合展示和运行较小程序
- 自举解释器相关验证由 `tests/self_host.rs` 覆盖
