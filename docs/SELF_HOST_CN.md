# BF 自举解释器

[`examples/bf_self_host.bfs`](../examples/bf_self_host.bfs) 是一份完全用
BFS（AmazingBF 自带的 Brainf Script DSL）写的 Brainfuck 解释器。`bfsc`
将其降级为 Brainfuck 文本，再交给 AmazingBF 执行——**用 BF 解释 BF**。

参考设计来自 LINUX DO 帖子 _《编码尝试：使用 Brainfuck 实现 Brainfuck
解释器》_，原文是用一种自定义的 C 子集（`bfrtc`）走类似的目标。本仓的实现
不照抄那条管线——BFS 已经提供类型化标量、数组、结构化控制流——但参考帖里
的核心性能想法（紧凑 opcode 编码、预先算好的 bracket 跳转表、热路径优先
派发）依然适用。

> 英文同伴文档位于 [`docs/SELF_HOST.md`](SELF_HOST.md)，两份保持同步。

## 流水线

```text
examples/bf_self_host.bfs
   │ bfsc                      (BFS → BF 文本，~26 MB)
   ▼
bf_self_host.bf
   │ AmazingBF (interpret | jit | tiered | compile)
   ▼
从 stdin 读用户的 BF 程序并执行
```

## I/O 约定

发到编译后解释器的 stdin：

```text
<bf_program>!<bf_program_input>
```

`!` 字节（或 EOF，即 `0xFF`）作为程序段的结束符。`!` 后面的字节喂给用户
程序的 `,` 操作。这正是参考帖里「程序与输入合并到同一条带上」的做法。

跑一段 BF 程序：

```bash
cargo build --release
./target/release/bfsc examples/bf_self_host.bfs -o /tmp/bfi.bf

# 把 BF 源 + 输入一起灌进去
printf '%s!%s' "$(cat tests/cases/1.bf | tr -d '\n')" '' \
  | ./target/release/AmazingBF /tmp/bfi.bf -q
# → Hello World!
```

只跑一次的话，`bfsc -c` 直接出原生可执行：

```bash
./target/release/bfsc examples/bf_self_host.bfs -c -O3 -o /tmp/bfi
printf '%s!' "$(cat tests/cases/1.bf | tr -d '\n')" | /tmp/bfi
```

## 设计

### 内存布局

五个缓冲区都是 u8 数组，长度选取保证 `bfsc` 走便宜的 1 字节索引路径
（`arr_len ≤ 256`）。声明顺序经过精挑细选——把热数组放在最靠近 temp 池的
位置，emit 出来的 goto 距离会显著缩短：

| 缓冲   | 长度 | 用途                                                     |
|--------|------|----------------------------------------------------------|
| `bstk` |   64 | Phase 2 预扫描时的 bracket 栈                            |
| `jmp`  |  256 | bracket 跳转表（fold 后的 PC）                           |
| `prog` |  256 | 编码后的 opcode 流（`prog_len` 之后留 0 作 halt sentinel）|
| `cnt`  |  256 | 与 `prog` 平行的 RLE 计数（仅 op 1‥4 使用）              |
| `tape` |  256 | 模拟 BF 条带                                             |

哨兵：`prog[prog_len]` 始终为 `0`，所以 PC 跑过末尾会落到 opcode `0` 而
干净���退出派发循环——热循环里不需要显式长度检查。Phase 1b 折叠完之后会
显式写一次 `prog[w] = 0`，否则被压掉的指令位置可能残留旧 opcode。

### 紧凑 opcode 编码

8 个 BF 操作符在加载期映射到 1..8 的紧凑字母表，再加 3 条超指令（9..11）
识别常见模式：

| BF / 模式         | Opcode | 备注                                   |
|-------------------|--------|----------------------------------------|
| `>`               | 1      | RLE — 一次走 `cnt[pc]` 步              |
| `<`               | 2      | RLE                                    |
| `+`               | 3      | RLE — `tape[ptr] += cnt[pc]`           |
| `-`               | 4      | RLE — `tape[ptr] -= cnt[pc]`           |
| `.`               | 5      |                                        |
| `,`               | 6      |                                        |
| `[`               | 7      |                                        |
| `]`               | 8      |                                        |
| `[-]` / `[+]`     | 9      | Zero — 直接替换循环体，单次派发        |
| `[<]`             | 10     | ScanLeft — 紧凑的内部循环              |
| `[>]`             | 11     | ScanRight                              |

opcode `0` 保留作 halt 哨兵。

BF 中和小常数比较远比和 ASCII 码比较便宜（每个 `==` 字面值大约展开成
`val` 个 `+`/`-` 字符），所以基于 1..11 的派发链比直接匹配原始字节大约
缩小到 1/6。

### Phase 1a — 边读入边做 RLE

读循环里持续保留刚才发射的最后一条 opcode。当本次读到的字符映射到
`>/</+/-` 之一，且与上一条 opcode 一致、`cnt[prog_len-1] < 255`，就只把
计数加一，不再追加新对。否则追加一对新的 `(op, cnt=1)`。原本一字一
slot 的 `+++++…` 长串现在压成一条——可用的程序长度上限随之放大。

### Phase 1b — 模式折叠

一次双指针扫描，识别 3 元素窗口 `[ X ]`，其中 `X` 必须 `cnt == 1` 且
`X ∈ {Add(1), Sub(1), Right(1), Left(1)}`：

- `[ Add(1) ]` / `[ Sub(1) ]` → `Zero` (9)
- `[ Right(1) ]` → `ScanRight` (11)
- `[ Left(1) ]` → `ScanLeft` (10)

Phase 1b 必须跑在 Phase 2 之前，bracket 跳转表要基于折叠后的索引。
被折叠的循环在运行期变成单条 super-op 派发，省掉了经典 BF 解释器需要在
3 条指令之间反复跳转的开销。

### Phase 2 — Bracket 预扫描

线性扫一遍 `prog`，把 `jmp` 填充成
`jmp[i_open] = i_close`、`jmp[i_close] = i_open`，扫描期间用 `bstk` 当
未配对 `[` 的栈。预扫之后，运行期每次 `[`/`]` 都是 O(1) 跳转。

### Phase 3 — 热路径派发

执行循环是按出现频率排序的 if/else 级联。RLE 类的分支（1..4）一次读取
`cnt[pc]`，把整条游程在一句 BFS 里搞定。模式分支（9..11）替换掉它们对应
的内部循环。无法识别的落到 halt 分支。

### 为什么不用更大的缓冲？

`bfsc::codegen::arr_read` / `arr_write` 现在已经支持当 `arr_len > 256`
时切到 2 字节索引路径，所以缓冲理论上可以扩。但实测里 2 字节路径要 emit
出多得多的 BF 文本——实验里 384 cell 的版本编出来 ~5 GB BF，再去喂给
AmazingBF 解析是不切实际的。256 + RLE+模式折叠的组合通常已经能撑住
500-1200 字节的 BF 源（远超 RLE 之前的 255 上限），同时把出来的 BF 控制
在 30 MB 以内。

## 限制

- **`prog_len ≤ 254`，是 fold 之后的指令数。** 也就是说，原始 BF 源能塞
  多大取决于它的可压缩性。`+++…`、`[-]` 清零、`[<]`/`[>]` 扫描这种重复模
  式多的程序，1000 字节出头也能装下。
- `tape_size = 256`，扫到 cell 255 之外时 `ptr` 会回卷。
- bracket 嵌套深度 ≤ 64。Phase 2 超出会使 `bstk` 越界，**未做检查**——
  实际 BF 程序基本到不了。
- 假设 bracket 平衡，未配对的 `]` 会让 `bsp` 下溢。
- 标准 benchmark `dbfi.b`（fold 后约 294 op）、`factor.b`（约 1206）、
  `mandelbrot.b`（约 3867）、`hanoi.b`（约 14863）目前依然超界，要等
  缓冲扩大才能跑。

## 性能数据

记录在主基准套件同一台机器上（Intel Core i9-14900K, Linux），
`cargo build --release`。新版本 BFS 编出来的 BF 文本约 26 MB（原来 RLE
之前是 13 MB）；解释模式跑一遍大约一半时间在 parse，另一半在派发。

| 模式                                            | `tests/cases/1.bf` (Hello World, 106 B) |
|-------------------------------------------------|------------------------------------------|
| `AmazingBF -m interpret` 跑 BFS-emitted BF      | ~10 s                                    |
| `AmazingBF -m compile -O3`（BF → x86_64）+ run  | ~12 s 编译 + 0.03 s 运行                 |

Hello-World 这一行主要由 parse cost 主导——RLE/模式折叠的实际收益要在
有大段 `+++…` 或 `[-]`/`[<]`/`[>]` 习语的程序上才看得出来。一段 792
字节、混合 `[-]` 和 `+++…` 长串的合成程序（远超原本 255 字符上限）现在
能在新版 self-host 上跑出正确结果，旧版会静默截断。

## 测试

`tests/self_host.rs` 先编译 BFS 源码，然后对 `tests/cases/*.bf` 中每个
fixture，把它过一遍 RLE+fold 估算 opcode 数（`effective_op_count`），
如果 `≤ 254` 就以 `<program>!<input>` 的形式喂给编译后的解释器（运行在
`AmazingBF -q` 下），输出和 `.out` 比对。超出预算的 case 跳过而不是
失败——后续缓冲扩容时这套测试还能继续生效。
