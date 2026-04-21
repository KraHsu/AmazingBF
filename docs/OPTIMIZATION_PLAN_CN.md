# AmazingBF 编译优化现代化方案

本文档为 AmazingBF 工具链的编译优化现代化路线图。它先整理现有 HIR / LIR / 后端 / 解释器的优化现状，再按实现依赖顺序列出待办项（TODO），供后续实施任务作为依据。

> 范围：HIR 层 analysis + passes、LIR 层 peephole 新层、Backend/codegen、Interpreter + runtime，加一节长期目标。
> 原则：向后兼容 O0–O3 CLI 语义；保持 `#![forbid(unsafe_code)]`；保持 `std`-only；分层边界不跨越。
> 英文版暂缺；按 `CLAUDE.md` 的双语规则，在任一 TODO 实际进入实施时再补 `docs/OPTIMIZATION_PLAN.md`。

---

## 1. 现状整理

### 1.1 HIR 层（`src/ir/hir.rs`, `src/ir/optimize.rs`）

- HIR 变体：`Move(isize)` / `Add(i32)` / `PutByte` / `GetByte` / `Zero` / `LinearMul(Vec<(isize,i32)>)` / `Loop(Vec<HirInst>)`
- **O0** `optimize_o0`（`optimize.rs:38-42`）：相邻 `Move/Add` 融合，单次遍历
- **O1** `optimize_o1`（`optimize.rs:46-49`）：O0 融合 + `try_scan_loop` / `is_byte_clear_loop` / `try_linear_loop`（`optimize.rs:136-199`）+ 局部 `ConstEnv`（`optimize.rs:202-254`）+ `push_o1` peephole（`optimize.rs:349-369`）
- **O2** `try_optimize_o2`（`optimize.rs:60-73`）：对 O1 不动点迭代，4096 上限
- **O3**：不是独立 HIR pass，在 `src/driver/run.rs:138-176` 做整程序折叠（无 `PutByte` → `exit(0)`；无 `GetByte` → 离线跑完再 `write + exit`）
- **分析基础设施**：仅 O1 的 `ConstEnv`（同一 block 内绝对 tape 索引上已知字节值），**无 SSA**、**无 dataflow**、**无 range / liveness**

### 1.2 LIR 层（`src/ir/lir.rs`, `src/ir/lower.rs`）

- LIR 变体：`PtrAdd` / `CellAdd` / `CellSet` / `LinearMul` / `Scan` / `PutByte` / `GetByte` / `Label` / `JumpIfZero` / `JumpIfNonZero`
- `lower_to_lir_block`（`lower.rs:48-82`）纯机械下降，只剔除零 delta
- **无 peephole、无 scheduling、无 bounds-check 聚合**

### 1.3 Backend（`src/backend/codegen.rs`, `src/backend/x86_64/`）

- 固定寄存器：`R12 = tape_base` / `R13 = data_ptr` / `R14 = tape_end` / `R15 = scratch`（`asm.rs:16-33`）
- 每个 `PtrAdd` ≈ 12 指令 fast path（`codegen.rs:78-107`），越界走 `ensure_tape_contains_r15`（`codegen.rs:442-563`）
- 每个 `.` / `,` 对应一个 `write` / `read` syscall 或 `WriteFile` / `ReadFile`（`codegen.rs:204-231`, `windows.rs:518-572`）
- 跳转一律 5 字节 `rel32`；无 `inc` / `dec` 选择；无 SIMD；无 codegen peephole

### 1.4 Interpreter + Runtime（`src/interp/engine.rs`, `src/runtime/`）

- 单层 `match` 派发（`engine.rs:66-132`），无 threaded dispatch / 无超级指令
- `Tape` 用 `Vec<u8>` 左右拼接（`tape.rs:34-209`），`Vec::resize` 同步增长；**与 `CLAUDE.md` 所述 “mmap + doubling” 不一致**
- 基准基础设施（E5）已落地：`benches/standard_suite.rs`（Criterion，matslina 套件的解释/执行）与 `benches/compile_levels.rs`（自定义 harness，`tests/cases/*.bf` 的编译+运行耗时总表）。`tests/compile_artifacts.rs` 仅做产物正确性校验

---

## 2. 现代化路线图

按实现依赖顺序分六个阶段：A 分析基建 → B HIR pass → C LIR peephole 新层 → D Backend/codegen → E Interpreter + runtime（可并行）→ F 长期目标（不进入近期依赖图）。

### Phase A — 分析基础设施（其它大部分 pass 的前提）

- **A1 符号 tape 状态**：为 HIR 引入 per-block 的符号 tape 状态，以 block 入口 `data_ptr` 为原点，记录每个已访问偏移上的 (known value, taint) 和 pointer 当前偏移。支持 `merge` 和 `clobber`（I/O、未分析的 `Loop` 触发）。扩展 O1 的 `ConstEnv` 而非替换。
  - 关键文件：`src/ir/optimize.rs`；新建 `src/ir/analysis/`
- **A2 Pointer-delta 抽象解释**：对任意 HIR 片段计算 `(min_off, max_off, net_delta)`。产出：
  ```rust
  struct LoopEffect {
      net_ptr_delta: Option<isize>,
      touched: Range<isize>,
      reads_cell: bool,
      writes_cell: bool,
      has_io: bool,
  }
  ```
  `LinearMul` 的识别条件可以用它重写得更通用。
- **A3 Cell 值抽象格**：每格 `Top | Const(u8) | NonZero | Zero`。A1 的 tape 状态升级为该格上的映射，为 DSE 和 known-zero loop 消除提供支撑。
- **A4 跨 block dataflow 框架**：forward data-flow 的通用骨架（worklist + transfer function）；`Loop` 将 block 作为 join 点。首个客户是 live-cell 分析。

**依赖：A1 → A2 → A3 → A4。**

### Phase B — HIR pass 扩充（依赖 Phase A）

- **B1 Dead store elimination**：基于 A3 + A4 的 live-cell 分析，消除后续被覆盖、中间没有读的 `Add` / `Zero`。当前 `push_o1` 只处理连续 `Add, Zero` 对。
- **B2 已知零格循环消除的跨 block 扩展**：O1 只在 block 入口 `ConstEnv` 做；A3 允许在 `Loop` 边界识别 `value_at_ptr == Zero` 并整块丢弃。
- **B3 LinearMul 泛化**：当前 `try_linear_loop`（`optimize.rs:136-168`）要求 net pointer delta 为 0 且头格 `-1`。借助 A2 放宽到：
  - 头格 `-k` 且 `gcd(k, 256) == 1`（仍保证终止）
  - 嵌套 `LinearMul` 体可在外层被识别（当前 body 含 `Loop` 立刻放弃）
  - 提供 `LinearMul` 的 fused copy 形式（因子仅 ±1 的列）
- **B4 Pointer postponement（指针延后 / 偏移化）**：业界标准命名（[Nayuki](https://www.nayuki.io/page/optimizing-brainfuck-compiler)、[matslina](https://github.com/matslina/bfoptimization) 称 “operation offsets”，bfc 称 “postponing movements”）。对任一 straight-line block（无 `Loop` / 无 I/O），以入口 `data_ptr` 为原点跟踪 virtual offset，把 `Move(d1); Add(k); Move(d2); Add(j); ...` 重写为按偏移聚合的读写，仅在 block 出口 / 进入 loop / 进入 I/O 前提交一次实 `Move`。这是后续 C3 displacement 形式的真正前提，也严格包含旧 “balanced-pointer canonicalization” 思路。可能需要新增 HIR 变体 `AddAt { off, delta }` / `SetAt { off, val }`，或将职责推迟到 LIR（见 C3）。
- **B5 Loop-invariant code motion**：A3 识别被 loop 读但不被改写的 cell；将其 loop-pre 赋值保留在 loop 外，避免反复 reload。BF 上实用性偏弱，但 SSA-化后几乎免费。
- **B6 小 loop 展开**：进入 loop 时头格值已知（A3）且 body 为 affine 时，编译期直接计算 `Add` / `Zero` 序列，不必生成 `LinearMul`。
- **B7 Deep balanced loop（K6 算法，选配）**：[Oizys](https://github.com/jjcmoon/Oizys) 提出的 K6 算法对嵌套 balanced loop 做统一分析，覆盖 `try_linear_loop` + `try_scan_loop` 无法触达的程序类。作为 B3 / B4 稳定后的研究性扩展。

**依赖：B1–B3 直接依赖 A；B4 是 C3 的前置；B5 / B6 可并行；B7 依赖 B3 + B4 + A2。**

### Phase C — LIR peephole 新层

目标：`src/ir/lower.rs` 之后新增 `src/ir/lir_opt.rs`，提供一轮无分析 peephole。

- **C1 LIR peephole 基础 pass** · **[已实现]**：合并相邻 `PtrAdd`、消除零 delta、折叠 `CellSet(0); CellAdd(k)` → `CellSet(k)`、`CellSet(a); CellSet(b)` → `CellSet(b)`。落地在 `src/ir/lir_opt.rs`，挂在 `lower_to_lir` 之后；`Label` / `JumpIfZero` / `JumpIfNonZero` / `Scan` / `LinearMul` / `PutByte` / `GetByte` 作为自然 barrier。
- **C2 Bounds-check hoisting / batching**：连续 `PtrAdd` 只在总 delta 的极值上做一次 bounds check（当前每个 `PtrAdd` 独立检查，`codegen.rs:78-107`）。需要向后端传递 “已检查区间” 标记，考虑在 LIR 引入 `PtrAddChecked { delta, lo_extent, hi_extent }` 或新 op。
- **C3 Displacement 形式下降**：B4 的 pointer postponement 输出的 `AddAt { off, delta }` / `SetAt` 直接降到 LIR 的 `CellAddAt { off, delta }` / `CellSetAt { off, val }`，后端发 `add byte [r13 + disp8], imm` / `mov byte [r13 + disp], imm`，省一次指针更新和一次 bounds check。x86_64 disp8 范围 ±127 恰好覆盖绝大多数 straight-line block 的偏移跨度；超出用 disp32。
- **C4 Scan / Zero 传递 size 提示**：让后端知道某个 `Scan` 之前的 tape 区段是否已检查过 bound，决定是否内联边界检查。

**依赖：C1 独立；C2 依赖 A2 的 range；C3 依赖 B4；C4 依赖 C2。**

### Phase D — Backend / codegen（依赖 Phase C 的语义保证）

- **D1 指令选择** · **[已实现]**：`CellAdd(±1)` → `inc` / `dec`（opcode `0xFE`）；`add/and/cmp r, imm` 立即数自动选 `0x83 + imm8`（4 字节）还是 `0x81 + imm32`（7 字节）；长 Jcc / JMP 由独立的 relaxation pass（`src/backend/x86_64/relax.rs`）迭代收缩到 rel8 短跳。
  - 文件：`src/backend/asm.rs`、`src/backend/x86_64/encode.rs`、`src/backend/x86_64/relax.rs`、`src/backend/codegen.rs`
- **D2 SIMD 专用形式**：
  - `Scan(±1)` → `rep scasb`（`al = 0`, `rdi = r13`, `rcx` 设足够大）配合一次 bounds 收紧。需确认 Windows ABI 对 `rep` 指令无特殊要求。
  - `Zero` 连续段（来自未来 pass 合并）→ `rep stosb`
  - `LinearMul` 因子为 ±1 的列 → `movzx + add`，配合 C3 的 displacement 形式下降
- **D3 Buffered I/O**：引入 runtime 侧 I/O buffer（如 4KB），`PutByte` 写 buffer，满或退出前 flush；`GetByte` 从 buffer 读。降低 syscall 开销一个数量级。需在 `src/runtime/io.rs` 增加 `BufferedStdIo`，backend 在 `PutByte` / `GetByte` 调用新 runtime 入口。
- **D4 最小寄存器分配器**：为 `LinearMul` / loop 头的乘数与 src 值引入 `rbx / rax / rcx` 的显式使用跟踪（当前 `codegen.rs:143-159` 手工硬编码）。不做通用 RA，仅做 “多余 mov 消除” 级别的局部 allocator。为 Phase F 更激进的 codegen 铺路。
- **D5 跳转对齐 + 分支提示**：loop 头对齐 16B；给 `JumpIfZero` 加 `2e` / `3e` 分支提示前缀（Intel 上已无效，AMD 仍解析）——优先级最低。

**依赖：D1、D3 独立；D2、D4 依赖 C；D5 与 D1 共享 `encode.rs`，可并行。**

### Phase E — Interpreter + runtime（与 A–D 基本独立，可并行）

- **E1 Superinstruction lowering**：HIR → interpreter 之间新增 bytecode 表示（如 `Vec<InterpOp>`，`InterpOp` 包含融合形式如 `MoveAdd(d, k)` / `ZeroMove(d)`），消除 `engine.rs:66-132` 的 hot-path 分派开销。
- **E2 Threaded dispatch**：在 E1 之上用 `fn(&mut State)` 指针数组代替 `match`，获得 computed-goto 近似效果（稳定 Rust 无 computed-goto）。每个 `InterpOp` 对应一个 tail-call 风格 handler。
- **E3 SIMD tape 操作**：`Zero` → `memset`；`LinearMul` 中因子 1 的列 → `copy_from_slice`。`Tape::move_ptr` 的 zero-fill 走 `Vec::resize` 已是 `memset`，但 `LinearMul` 内部仍是标量（`engine.rs:103-111`）。
- **E4 Tape 后端重构** · **[已实现，方案调整]**：原方案要求 mmap + centered-copy，但与 `#![forbid(unsafe_code)]` 冲突。改为在现有 `Vec<u8>` 左右拼接布局上换上几何倍增（`new_len = max(needed, old_len * 2)`，左半因初始空载另设 8 字节下限）：均摊 O(1) 每访问格，避免单步走过边界触发 O(n) resize。`TapeStats::right_growth` 更名为 `right_grew_bytes` 以明确语义。后续若决定为共享 backend tape 再引入 mmap 版本，可在不破坏 forbid(unsafe) 的前提下通过 runtime feature flag 隔离。
- **E5 Criterion 微基准套件** · **[已实现]**：新增 `benches/`，采用 [matslina 标准基准集](https://github.com/matslina/bfoptimization) 的子集——**factor.b**、**mandelbrot.b**、**hanoi.b**、**dbfi.b**、**long.b** 以及 **awib-0.4.b**。按 O0 / O1 / O2 / O3 × (interpret, compile+run) 交叉衡量。这套程序覆盖不同 contraction 比例（40%–75%）和不同 hot-loop 模式，是 BF 优化文献的既定 benchmark；参考文献给出的参考加速范围：hanoi.b ≈ 130×、mandelbrot.b 数十倍、awib-0.4 ≈ 2.4×（全部优化 vs 无优化）。作为 A–D 所有 pass 的回归衡量基线。

> **E5 应最先落地**：一切后续优化的收益需要通过它量化。

**依赖：E5 无依赖，最先；E1 → E2；E3 无依赖；E4 与 D3 的 I/O 改造可放同一个里程碑。**

### Phase F — 长期目标（不进入近期依赖图）

- **F1 Tiered JIT**：解释器采集 loop trip count，热点处走 backend 生成的机器码。两条实现路径：
  - 自写（沿用现有 `src/backend/x86_64/encode.rs`）：代码闭环，但 `mmap(PROT_EXEC)` 必须打破 `#![forbid(unsafe_code)]`，需明确豁免范围。
  - [Cranelift](https://cranelift.dev/) 作为 JIT 后端：成熟的 codegen 框架，已被多个 BF JIT 案例（如 [Rodrigodd 的 Part 3](https://rodrigodd.github.io/2022/11/26/bf_compiler-part3.html)）使用，但破坏 “零运行时依赖” 承诺；与 F4 LLVM 的抉择类似。
- **F2 ARM64 后端**：Linux aarch64 + macOS arm64。寄存器约定需重设（`x12/x13/x14/x15` 映射），encode 层完全新写。
- **F3 macOS Mach-O x86_64 后端**：相对 ELF 工作量小，但 syscall 号变化，需增加 `LC_SEGMENT_64` / `LC_MAIN` 的文件格式代码。
- **F4 LLVM 后端（可选）**：代价是破坏“零运行时依赖”承诺；作为可开关的 feature flag 存在。
- **F5 增量编译缓存**：对固定 `.bf` 源缓存 HIR / LIR / obj，结合内容哈希。只有当 E5 表明编译时长占比明显时才值得。

---

## 3. 依赖总览

```
E5 (bench)  ──────────────────────────────────┐
                                              ▼
A1 → A2 → A3 → A4                        （回归衡量）
  │    │    │    │
  │    │    │    └→ B1 (DSE)
  │    │    └→ B2 (zero-loop), B5 (LICM), B6 (unroll)
  │    └→ B3 (LinearMul 泛化), B4 (pointer postponement), B7 (K6)
  │         │
  │         └→ C3 (displacement) → D2 / D4
  │
  └→ C2 (bounds batching) → D2 (SIMD)

C1 (LIR peephole)、D1 (指令选择)、D3 (buffered I/O)、
E1 / E2 (superinstruction + threaded dispatch)、
E3 (SIMD tape)、E4 (tape 倍增) 均可并行启动。
已落地：E5、C1、D1、E4。

Phase F 全部不在近期依赖图内。
```

---

## 4. 关键改动文件清单

| 层 | 文件 | 涉及阶段 |
|---|---|---|
| HIR | `src/ir/hir.rs` | B3 / B4 若需新变体 |
| HIR | `src/ir/optimize.rs` | Phase A / B 主修改点 |
| HIR | `src/ir/analysis/`（新） | A1–A4 骨架 |
| LIR | `src/ir/lir.rs`, `src/ir/lower.rs` | B / C 可能新增 `PtrAddChecked` / `CellAddAt` |
| LIR | `src/ir/lir_opt.rs`（新） | Phase C 主场 |
| Backend | `src/backend/codegen.rs`, `src/backend/x86_64/encode.rs`, `src/backend/asm.rs` | Phase D |
| Backend | `src/backend/x86_64/elf.rs`, `src/backend/x86_64/windows.rs` | D3 buffered I/O |
| Runtime | `src/interp/engine.rs`, `src/runtime/{tape,io,host}.rs` | Phase E |
| Bench | `benches/`（新） | E5 |
| Tests | `tests/cases_pipeline.rs`, `tests/compile_artifacts.rs` | 每阶段新增 pass 后回归 |

---

## 5. 验证方法

- 每个 TODO 合入前：`cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
- 正确性回归：`tests/cases_pipeline.rs`、`tests/bfsc_pipeline.rs`、`tests/windows_target.rs` 全绿
- 性能回归：E5 落地后，每个 Phase B / C / D pass 均需提交 criterion 对比（factor / mandelbrot / hanoi / dbfi / long / awib-0.4 基准集，解释和编译各一组）
- 慢基准：`cargo bench --bench compile_levels` 对比 pre / post
- 二进制大小：对 D1 / D2 记录编译产物 `.text` 字节数变化

---

## 6. 非目标

- 不调整现有 O0–O3 CLI flag 的语义（不改动 `src/cli.rs` / `src/driver/config.rs`）；新的优化按既有层级归属。
- 不引入第三方 runtime 依赖（保持 `std`-only）。
- 不破坏 `#![forbid(unsafe_code)]`；Phase F1 的 JIT 会单独讨论豁免范围，不属于本路线图近期部分。
- 本次不翻译英文版 `docs/OPTIMIZATION_PLAN.md`；在真正实施某个 TODO 时再按 `CLAUDE.md` 双语规则补齐。

---

## 7. 参考资料 / Prior Art

路线图中的关键技术并非首创；以下是本文档各 TODO 的业界来源和对照实现，便于实施时直接研读成熟做法。

### 核心综述

- **[matslina, "Brainfuck Optimization Strategies"](http://calmerthanyouare.org/2015/01/07/optimizing-brainfuck.html)** — BF 优化领域的奠基文献。定义了现代 BF 编译器的优化序列：contraction → clear loops → copy/multiply loops → operation offsets → scan loops。配套仓库 [matslina/bfoptimization](https://github.com/matslina/bfoptimization) 提供参考实现与基准数据。
- **[Nayuki, "Optimizing Brainfuck Compiler"](https://www.nayuki.io/page/optimizing-brainfuck-compiler)** — 给 pointer postponement 和 balanced-loop 判定下了最清晰的形式化定义；B4 / C3 的语义直接对照此文。

### 参考实现（按本路线图相关性排序）

- **[Wilfred/bfc](https://github.com/Wilfred/bfc)（Rust）** — “industrial-grade” 定位，有 position-preserving IR、idempotence / observational-equivalence 级别的优化回归测试。其优化 pass 列表（fusing increments、fusing movements、fusing movements into adds、postponing movements、simple loops、assign followed by add、complex loops）与本路线图 Phase B–C 几乎一一对应，是最接近的 Rust 生态对标。
- **[matslina/awib](https://github.com/matslina/awib)** — BF 写成的 BF 编译器，6 种后端。对 codegen 选择和基准比较有参考价值。
- **[jjcmoon/Oizys](https://github.com/jjcmoon/Oizys)** — 提出 K6 算法（B7 来源），处理深度嵌套 balanced loop。
- **[Rodrigodd 的 BF 编译三部曲](https://rodrigodd.github.io/2022/10/21/bf_compiler-part1.html)** — Part 1 优化解释器，Part 2 Singlepass JIT，Part 3 Cranelift JIT。Phase E（解释器）和 Phase F1（JIT）的工程对照。
- **[danthedaniel/BF-JIT](https://github.com/danthedaniel/BF-JIT)（Rust）** — AOT + JIT 混合编译：小 loop 立即编译、热点 loop 延迟编译。F1 tiered JIT 策略的具体参照。
- **[Brian Quinlan 的 brainfuck-jit](https://github.com/brianquinlan/brainfuck-jit)** — Operation offsets 直接落到 x86-64 的极简示例。

### 全编译器列表

- **[Esolang: Brainfuck implementations](https://esolangs.org/wiki/Brainfuck_implementations)** — 社区维护的全景列表，含 Tritium / libbf / esotope-bfc / ssbi / Hamster / bfcfs 等本路线图未单独列出但值得对照的实现。

### 基准程序来源

- factor.b / hanoi.b / mandelbrot.b / dbfi.b / long.b：[matslina/bfoptimization](https://github.com/matslina/bfoptimization) 配套。
- awib-0.4.b：[matslina/awib](https://github.com/matslina/awib) 自身作为 benchmark 输入。

这些程序在既有文献中反复出现，使用它们意味着本工具链的优化效果可以直接与 bfc / awib / Oizys / Tritium 横向比较，不必自建一套无从对标的 benchmark。
