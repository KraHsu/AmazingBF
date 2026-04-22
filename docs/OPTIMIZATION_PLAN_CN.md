# AmazingBF 编译优化现代化方案

本文档为 AmazingBF 工具链的编译优化现代化路线图。它先整理现有 HIR / LIR / 后端 / 解释器的优化现状，再按实现依赖顺序列出待办项（TODO），供后续实施任务作为依据。

> 范围：HIR 层 analysis + passes、LIR 层 peephole 新层、Backend/codegen、Interpreter + runtime，加一节长期目标。
> 原则：向后兼容 O0–O3 CLI 语义；保持 `#![forbid(unsafe_code)]`；保持 `std`-only；分层边界不跨越。
> 双语：英文同伴版位于 `docs/OPTIMIZATION_PLAN.md`，与本文件同步更新。

---

## 1. 现状整理

### 1.1 HIR 层（`src/ir/hir.rs`, `src/ir/optimize.rs`）

- HIR 变体：`Move(isize)` / `Add(i32)` / `PutByte` / `GetByte` / `Zero` / `LinearMul(Vec<(isize,i32)>)` / `Loop(Vec<HirInst>)`
- **O0** `optimize_o0`（`optimize.rs:38-42`）：相邻 `Move/Add` 融合，单次遍历
- **O1** `optimize_o1`（`optimize.rs:60-68`）：O0 融合 + `try_scan_loop` / `is_byte_clear_loop` / `try_linear_loop`（`optimize.rs:151-197`）+ 基于 A1 `TapeState` 的常量传播（`optimize.rs:219-307`）+ `push_o1` peephole（`optimize.rs:309-329`）+ **B1 DSE**（`src/ir/dse.rs`）串联在管线末端
- **O2** `try_optimize_o2`（`optimize.rs:74-87`）：对 O1 不动点迭代（含 DSE），4096 上限
- **O3**：不是独立 HIR pass，在 `src/driver/run.rs:138-176` 做整程序折叠（无 `PutByte` → `exit(0)`；无 `GetByte` → 离线跑完再 `write + exit`）
- **分析基础设施**：`src/ir/analysis/` 下的 A1 `TapeState` / A2 `LoopEffect` / A3 `CellLattice` 四点格 / A4 `run_forward` forward dataflow 骨架全部落地（57 单元测试覆盖）。A1 + A3 已接入 O1 常量传播（A3 的 `add_wrapping` 驱动 `TapeState::apply` 的 `Add` 分支）；A3 `is_zero` 进一步驱动 B2 `Loop` 丢弃与 B6 小循环展开。A2 / A4 仍是待接入的基础设施——`run_forward` 的跨循环 fixpoint 精度留给 B5 LICM；`LoopEffect` 是 B7 K6 的前置

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

- **E1 / E2 已落地**：HIR 先经 `src/interp/lower.rs::lower_hir_to_bytecode` 下降到 `src/interp/bytecode.rs::InterpOp` 超级指令流（含 `MoveAdd` / `ZeroMove` 融合，`LoopStart` / `LoopEnd` 携绝对 pc），再由 `engine.rs::exec_bytecode` 用 `src/interp/handlers.rs` 的 tag-indexed 函数指针表派发；原递归 `exec_block` + 单层 `match` 已整体替换
- `Tape` 用 `Vec<u8>` 左右拼接（`tape.rs:34-209`），`Vec::resize` 同步增长；**与 `CLAUDE.md` 所述 “mmap + doubling” 不一致**
- 基准基础设施（E5）已落地：`benches/standard_suite.rs`（Criterion，matslina 套件的解释/执行）与 `benches/compile_levels.rs`（自定义 harness，`tests/cases/*.bf` 的编译+运行耗时总表）。`tests/compile_artifacts.rs` 仅做产物正确性校验

---

## 2. 现代化路线图

按实现依赖顺序分六个阶段：A 分析基建 → B HIR pass → C LIR peephole 新层 → D Backend/codegen → E Interpreter + runtime（可并行）→ F 长期目标（不进入近期依赖图）。

### Phase A — 分析基础设施（其它大部分 pass 的前提）

- **A1 符号 tape 状态** · **[已实现]**：`src/ir/analysis/tape_state.rs` 的 `TapeState` 以 block 入口 `data_ptr` 为原点，`BTreeMap<isize, CellLattice>` 记录每个已访问偏移上的格值与当前 ptr 偏移，含 `merge_in_place`（ptr 不一致退化为 pessimistic）、`clobber_all`（I/O 之外的 `Loop` / `Scan` / `LinearMul` 触发）。已接入 O1 的 `optimize_block_o1_with_parent_env`，通过 `ConstPropXfer: Transfer<TapeState>` 驱动符号执行。
- **A2 Pointer-delta 抽象解释** · **[已实现]**：`src/ir/analysis/loop_effect.rs` 的 `LoopEffect::analyze` 产出 `{ net_ptr_delta, touched, reads_cell, writes_cell, has_io }`，辅以 `pointer_delta_range` 计算 `(min_off, max_off, net_delta)`。目前作为骨架，`try_linear_loop` 尚未迁移到它上面（B3 会接入）。
- **A3 Cell 值抽象格** · **[已实现]**：`src/ir/analysis/lattice.rs` 的 `CellLattice { Top, NonZero, Zero, Const(u8) }` 提供 `meet` / `add_wrapping` / `is_zero` / `is_nonzero` / `known_u8`。`TapeState::apply` 的 `Add(k)` 分支改为 `current.add_wrapping(k)`，把转移从 "literal-equivalence" 升级到真正的格语义；`NonZero` 在 `merge_in_place` 产生的 cross-block 事实中得以保留。
- **A4 跨 block dataflow 框架** · **[已实现]**：`src/ir/analysis/dataflow.rs` 提供 `Fact` / `Transfer` 两个 trait 与 `run_forward` 驱动；`transfer_loop` 以 64 iters 为上限做格上的不动点迭代，fail-safe 退回 `Fact::bottom()`。对 `Option<TapeState>` 与 `TapeState` 两种 Fact 的实现到位。目前 `run_forward` 仅在单测中使用，首个正式消费者（live-cell 分析，为 B5 LICM 铺路）待后续 phase 接入。

**依赖：A1 → A2 → A3 → A4 均已就绪，Phase B 可展开。**

### Phase B — HIR pass 扩充（依赖 Phase A）

- **B1 Dead store elimination** · **[已实现]**：`src/ir/dse.rs` 的 `dead_store_elimination` 做前向句法重写——虚拟指针 + `BTreeMap<isize, usize>` pending 写集。某 offset 的写被后续同 offset 写覆盖且中间无读时丢弃前者。`Loop` / `Scan` / `LinearMul` 作为 barrier 清空 pending 并递归进入 `Loop` body；`PutByte` / `Add` 提交（读到）前写，`GetByte` / `Zero` 无条件覆盖前写，`GetByte` 本身不可丢（输入副作用）。覆盖 `push_o1` 错过的 Move 间隔、GetByte 覆盖两类场景。串联在 `optimize_o1` 末端，O2 不动点循环自动受益。最终实现纯句法，不消费 A4 `run_forward`（A3 `CellLattice` 留给后续 B2 / B5 的 cross-block 变体使用）。
  - 文件：`src/ir/dse.rs`；集成点 `src/ir/optimize.rs:optimize_o1`
- **B2 已知零格循环消除的跨 block 扩展** · **[已实现]**：`src/ir/optimize.rs` 在 `Loop` 分支入口处，先于 `inner.is_empty()` 与 `try_loop_specialize` 判断 `env.value_at_ptr() == Some(0)`——`TapeState` 的前向 env 由整个父 block 贯穿携带，把 "Loop 进入时头格证明为零" 从原 `empty body + v==0` 单一路径扩到任意 body 形态（包括之前仍生成无效 `LinearMul` / `Scan` / `Loop` 的情形）。覆盖的场景：程序起始 `TapeState::new_program()` 令 `cell[0]=Const(0)`、`Add(5); Add(-5); [...]` 之类算术归零、显式 `Zero; [...]`、嵌套 `Loop` 外层零值等。实现未引入 `run_forward`——当前父 block 已是单线程前向走位，B2 只补丢弃决策；`run_forward` 的跨循环 fixpoint 精度留给后续 B5 LICM 作首个正式消费者。
  - 文件：`src/ir/optimize.rs`（6 B2 单测 + 9 既有 Loop 路径测试改以 `GetByte` 前缀阻 B2 以继续覆盖 specialisation 分支）
- **B3 LinearMul 泛化（头格 gcd 放宽）** · **[已实现]**：`src/ir/optimize.rs` 的 `try_linear_loop` 现接受任意奇数头格 `d0`——通过 `invmod_256(d0)`（扩展欧几里得）在编译期算出迭代次数的乘法逆元，将 body 中每个 factor 统一缩放到 `factor * invmod(-d0, 256)` mod 256 后沿用既有 `LinearMul` 数据结构，不新增变体。偶数头格仍被拒（`gcd(|d0| mod 256, 256) ≠ 1` 时循环要么非终止、要么非整数迭代）。`is_byte_clear_loop` 同步放宽，识别任何奇数步长的 `[-]` / `[--]` 等价形式。嵌套 `LinearMul` body 与 ±1-only fused copy 两种形态暂未纳入，作为 B6 / B7 的延伸。
  - 文件：`src/ir/optimize.rs`（9 单测：`invmod_256` 全表覆盖、奇负头格、多 offset、偶数拒绝）
- **B4 Pointer postponement（指针延后 / 偏移化）** · **[已实现]**：业界标准命名（[Nayuki](https://www.nayuki.io/page/optimizing-brainfuck-compiler)、[matslina](https://github.com/matslina/bfoptimization) 称 “operation offsets”，bfc 称 “postponing movements”）。最终落实为 LIR-only 方案（不新增 HIR 变体，避免污染 HIR interpreter 与现有 pattern detector）：`src/ir/lir_postpone.rs` 的 `postpone_pointer_adds` 按 straight-line window 累积 `virt_ptr: isize` 与 `pending: BTreeMap<isize, PendingOp>`，遇到 barrier（`Label` / `JumpIfZero` / `JumpIfNonZero` / `Scan` / `LinearMul` / `PutByte` / `GetByte` 以及上一次 pass 残留的 `CellAddAt` / `CellSetAt`）或 `virt_ptr` 要越 disp8 边界时触发 flush；flush 前按 `(lo, hi)` 两极发探针 `PtrAdd`，由后端的 `ensure_tape_contains_r15` 对接 tape 倍增，利用 tape 映射的连续性保证窗口内所有偏移都已被 bounds check。disp 范围限定在 `[-127, 127]`（disp32 推迟到 C2 落地后放宽）。挂在 `-O1` 及以上的 `optimize_lir(lower_to_lir(hir))` 之前。
  - 文件：`src/ir/lir_postpone.rs`（18 单测含安全证明）；集成点 `src/driver/run.rs:build_optimized_lir`
- **B5 Loop-invariant code motion**：A3 识别被 loop 读但不被改写的 cell；将其 loop-pre 赋值保留在 loop 外，避免反复 reload。BF 上实用性偏弱，但 SSA-化后几乎免费。
- **B6 小 loop 展开** · **[已实现]**：`src/ir/optimize.rs::try_unroll_known_head` 在 `Loop` 分支的 `try_loop_specialize` 之前检查 `env.value_at_ptr()`——值已知（`CellLattice::Const(v)`）且 `v != 0`、body 通过 `try_linear_loop` 时，按相对 `Move` 形式编译期展开：对每个 `(off, f)` 发 `Move(step); Add((v * f) as i8 as i32)`（零 delta 跳过、指针相对游走），末尾 `Move(-cur); Zero`。替代原先落入 `LinearMul` 的运行期 `*p * factor` 乘法。`v == 0` 继续走 `try_loop_specialize`（由 B2 commit 3 负责整块丢弃）；头格 `Top` / `NonZero` 的老程序依然走 `LinearMul` 无回归。`try_loop_specialize` 的 `Scan` / `is_byte_clear_loop` 路径不受影响——它们的 body 形态 `try_linear_loop` 会拒绝或返回空 factors，B6 自然退回。
  - 文件：`src/ir/optimize.rs`（6 单测：单 offset、多 offset、i8 canonicalisation、未知头、empty body 回归、v==0 pin B2 前行为）
- **B7 Deep balanced loop（K6 算法，选配）**：[Oizys](https://github.com/jjcmoon/Oizys) 提出的 K6 算法对嵌套 balanced loop 做统一分析，覆盖 `try_linear_loop` + `try_scan_loop` 无法触达的程序类。作为 B3 / B4 稳定后的研究性扩展。

**依赖：B1 / B2 / B3 / B4 / B6 已落地；B5 依赖 A3；B7 依赖 B3 + B4 + A2。**

### Phase C — LIR peephole 新层

目标：`src/ir/lower.rs` 之后新增 `src/ir/lir_opt.rs`，提供一轮无分析 peephole。

- **C1 LIR peephole 基础 pass** · **[已实现]**：合并相邻 `PtrAdd`、消除零 delta、折叠 `CellSet(0); CellAdd(k)` → `CellSet(k)`、`CellSet(a); CellSet(b)` → `CellSet(b)`。落地在 `src/ir/lir_opt.rs`，挂在 `lower_to_lir` 之后；`Label` / `JumpIfZero` / `JumpIfNonZero` / `Scan` / `LinearMul` / `PutByte` / `GetByte` 作为自然 barrier。
- **C2 Bounds-check hoisting / batching** · **[已实现]**：LIR 新增 `LirInst::PtrAddChecked { delta, lo_extent, hi_extent }`（`src/ir/lir.rs`）承载 "已在 `[delta + lo_extent, delta + hi_extent]` 区间内完成 bounds check" 的语义；`lo_extent == hi_extent == 0` 退化为旧 `PtrAdd`。B4 的 `lir_postpone.rs` 原先发两条探针 `PtrAdd(lo) / PtrAdd(hi)` 现合为一条 `PtrAddChecked`；`CellAddAt.off` / `CellSetAt.off` 从 `i8` 放宽到 `isize`，emitter 按 off 范围自动选 disp8 / disp32 变体（`src/backend/asm.rs::{AddMem8ImmDisp32, MovMem8ImmDisp32}`、`src/backend/x86_64/encode.rs::emit_mem8_disp32`）。后端在 `src/backend/codegen.rs` 维护 "已验证窗口" 状态机：`CellAdd*` / `CellSet*` / `ZeroRun` 对 r13 透明，`PtrAdd` 在窗口内省略 bounds check，`Label` / `Jump*` / `Scan*` / `LinearMul` / `PutByte` / `GetByte` 清空窗口。C1 peephole 扩展 `PtrAddChecked` 合并 / 吸收规则（叠加 delta、区间取并）。
  - 文件：`src/ir/lir.rs`、`src/ir/lir_postpone.rs`、`src/ir/lir_opt.rs`、`src/backend/codegen.rs`、`src/backend/asm.rs`、`src/backend/x86_64/encode.rs`
- **C3 Displacement 形式下降** · **[已实现]**：`LirInst::CellAddAt { off, delta }` / `CellSetAt { off, val }` 两个新变体（`src/ir/lir.rs`）承载 B4 flush 产物；后端在 `src/backend/codegen.rs` 将其翻译为 `AsmInst::AddMem8ImmDisp8` / `MovMem8ImmDisp8`（`src/backend/asm.rs`），编码由 `src/backend/x86_64/encode.rs` 的 `emit_mem8_disp8` 发出 `add byte [r13 + disp8], imm8` / `mov byte [r13 + disp8], imm8`（ModRM mod=01，以 R13 为基址时 `(rm & 7) == 5` 避开 SIB 歧义，对应机器码 `49 80 45 <disp> <imm>` / `49 C6 45 <disp> <imm>`）。`off == 0` 在 codegen 中 `debug_assert!` 已被 B4 canonicalise 回 `CellAdd` / `CellSet`（保留 D1 的 inc/dec 短形）；disp 通过 `i8::try_from` 断言 ∈ `[-128, 127]`。disp32 推迟到 C2 落地——原因：超 disp8 的偏移必须配合区间 bounds-check，否则 `ensure_tape_contains_r15` 的单点语义被打破。LIR peephole (`src/ir/lir_opt.rs`) 扩展了 4 条同 offset 折叠规则（`CellAddAt;CellAddAt`、`CellSetAt;CellAddAt`、`CellSetAt;CellSetAt`、`CellAddAt;CellSetAt`），跨 offset 不合并。
  - 文件：`src/ir/lir.rs`、`src/backend/asm.rs`、`src/backend/x86_64/encode.rs`、`src/backend/codegen.rs`、`src/ir/lir_opt.rs`
- **C4 Scan / Zero 传递 size 提示** · **[已实现]**：`src/ir/lir_scan_hint.rs` 的 `promote_scan_hints` 沿用与 C2 后端等价的 "已验证窗口" 状态机，识别每条 `Scan` 之前仍在有效的 bounds-check 覆盖——当窗口沿 `Scan` 方向有正向 extent 时，升格为 `LirInst::ScanWithHint { dir, hint_bytes }`；覆盖为 0 则保留原 `Scan` 走慢路径。后端在 `ScanWithHint` 的循环体内用 `inc r13` / `dec r13`（单字节，不做 cmp）迭代，循环退出时再发一次完整 `PtrAddChecked` 校准。`Zero` 连续段由 C1 peephole 合并为 `LirInst::ZeroRun { start: i32, count: u32 }`（disp32 形式），D2 未落地前后端仍逐字节 zero，但 LIR 形式已就位。
  - 文件：`src/ir/lir.rs`、`src/ir/lir_scan_hint.rs`（新，12 单测）、`src/ir/lir_opt.rs`、`src/ir/lower.rs`、`src/backend/codegen.rs`

**依赖：C1 独立；C2 依赖 A2 的 range；C3 依赖 B4；C4 依赖 C2。**

### Phase D — Backend / codegen（依赖 Phase C 的语义保证）

- **D1 指令选择** · **[已实现]**：`CellAdd(±1)` → `inc` / `dec`（opcode `0xFE`）；`add/and/cmp r, imm` 立即数自动选 `0x83 + imm8`（4 字节）还是 `0x81 + imm32`（7 字节）；长 Jcc / JMP 由独立的 relaxation pass（`src/backend/x86_64/relax.rs`）迭代收缩到 rel8 短跳。
  - 文件：`src/backend/asm.rs`、`src/backend/x86_64/encode.rs`、`src/backend/x86_64/relax.rs`、`src/backend/codegen.rs`
- **D2 SIMD 专用形式**：
  - `Scan(±1)` → `rep scasb`（`al = 0`, `rdi = r13`, `rcx` 设足够大）配合一次 bounds 收紧。需确认 Windows ABI 对 `rep` 指令无特殊要求。
  - `Zero` 连续段（来自未来 pass 合并）→ `rep stosb`
  - `LinearMul` 因子为 ±1 的列 → `movzx + add`，配合 C3 的 displacement 形式下降
- **D3 Buffered I/O** · **[解释器 + Linux 后端已实现 / Windows 后端待落地]**：
  - **解释器侧**：`src/runtime/io.rs::BufferedStdIo` 以 4 KiB `BufWriter<Stdout>` + `BufReader<Stdin>` 包住进程 stdio；`RuntimeIo` trait 新增默认 `flush()` 方法（no-op），`BufferedStdIo::flush` 通过 `BufWriter::flush` 把延迟 flush 错误经 `IoError::WriteError → RuntimeError::Io` 上抛，避免 `Drop` 中吞错。`Interpreter::run()` 尾部显式调用 `io.flush()?`，按 "exec 错误优先，成功后才报告 flush 错误" 排序。CLI `run_interpret` 默认构造 `BufferedStdIo::new()`。
  - **Linux 后端侧**：`src/backend/codegen.rs` 新增 `emit_init_output_buffer`（`mmap` 4 KiB 匿名区，`Rbx = buffer_base = 写指针`，`Rbp = buffer_base + 4096 = 结束哨兵`）与 `emit_flush_output`（helper 子程序，`lea rsi, [rbp - 4096]` 恢复 base，发 `write(1, base, rbx - base)` 后重置 `rbx`）。`PutByte` 从 5 指令 inline syscall（~42 字节）改为 `mov al, [r13]; mov [rbx], al; add rbx, 1; cmp rbx, rbp; jne skip; call flush_output; skip:`（~20 字节热路径、1/4096 触发 syscall）。`GetByte` 前 `call flush_output` 以便交互式提示在 `,` 可能阻塞前可见；`exit(0)` 前同样 flush。新 `AsmInst::{MovAlMemR13, MovMemRbxAl}` 变体 + 编码与 3 条编码单测；`Reg64::Rbp` 首次被本后端使用，加入 reg_num / Display 表。
  - **Windows 后端侧（待）**：`src/backend/x86_64/windows.rs` 的 `emit_put_byte` / `emit_get_byte` 仍走 `WriteFile` / `ReadFile` IAT 调用，每字节一次。后续 commit 以相同语义改造。
  - 文件：`src/runtime/io.rs`、`src/driver/run.rs`、`src/interp/engine.rs`、`src/backend/asm.rs`、`src/backend/codegen.rs`、`src/backend/x86_64/encode.rs`、`src/backend/x86_64/debug.rs`；测试 `tests/buffered_io.rs`（>4 KiB 解释器 + Linux 编译两条 + `,` EOF 回 255）、`src/backend/x86_64/encode.rs` 3 单测
- **D4 最小寄存器分配器**：为 `LinearMul` / loop 头的乘数与 src 值引入 `rbx / rax / rcx` 的显式使用跟踪（当前 `codegen.rs:143-159` 手工硬编码）。不做通用 RA，仅做 “多余 mov 消除” 级别的局部 allocator。为 Phase F 更激进的 codegen 铺路。
- **D5 分支提示前缀 + loop 头对齐** · **[分支提示已实现 / loop 头 16B 对齐暂缓]**：`src/backend/x86_64/encode.rs` 给 `Jz` / `JzShort` 前置 `0x2E`（not-taken hint，对应 BF `[` 通常进入 loop 体的 not-taken 语义），给 `Jnz` / `JnzShort` 前置 `0x3E`（taken hint，对应 BF `]` 常常回跳继续循环）。长形从 6 字节增至 7 字节，短形从 2 字节增至 3 字节；`relax.rs` 引入 `short_form_len(inst)` 让 rel8 偏移计算区分带 hint 的 `JzShort` / `JnzShort`（3 字节）和其它短跳（2 字节）。Intel 自 Netburst 起忽略这组前缀，AMD 仍按静态预测解析；都属合法无副作用前缀，所以不会破坏任何微架构。16B loop 头对齐需要引入可变长度 `Align` 伪指令 + 让 relaxation 在 jump 收缩与 align 填充之间迭代至不动点，改动面与 D5 自述的 "ROI 不显著" 不相称，按 CN/EN 路线图 "优先级最低" 的注释暂不落地。
  - 文件：`src/backend/x86_64/encode.rs`、`src/backend/x86_64/relax.rs`（`encoder_emits_three_bytes_for_hinted_short_jz` 单测）

**依赖：D1、D3 独立；D2、D4 依赖 C；D5 与 D1 共享 `encode.rs`，可并行。**

### Phase E — Interpreter + runtime（与 A–D 基本独立，可并行）

- **E1 Superinstruction lowering** · **[已实现]**：新增 `src/interp/bytecode.rs` 定义 `InterpOp`（含融合形式 `MoveAdd { d, k }` / `ZeroMove(d)`）、`LinearMulPlan`（紧凑 `Box<[(i32, i16)]>` factors、`Arc` 共享避免 O2 fixed-point 复制后的反复 clone）与 `InterpProgram`；新增 `src/interp/lower.rs::lower_hir_to_bytecode` 做 HIR → InterpOp 下降，单 pass 完成 `Move; Add → MoveAdd` / `Zero; Move → ZeroMove` 融合以及 `Loop` → `LoopStart { end_pc } / LoopEnd { start_pc }` 绝对 pc 回填（back-patch 栈由 `Vec<u32>` 维护）。`engine.rs::exec_bytecode` 从递归 `exec_block` 改为平坦 pc-indexed 派发，`[` / `]` 成为单次比较 + 绝对跳转，不再走 Rust frame。
  - 文件：`src/interp/bytecode.rs`（新）、`src/interp/lower.rs`（新，13 单测）、`src/interp/engine.rs`、`src/interp/mod.rs`
- **E2 Threaded dispatch** · **[已实现]**：`InterpOp::tag()` 返回稠密 opcode 索引（安全 `match` 实现，因 `#![forbid(unsafe_code)]` 禁用 `mem::transmute`/repr 透视）；新增 `src/interp/handlers.rs`，11 个 `fn(&mut Interpreter<I, H>, &InterpOp, usize) -> Result<usize, RuntimeError>` handler 构成 `dispatch_table::<I, H>() -> [Handler<I, H>; INTERP_OP_TAG_COUNT]`。`engine.rs::exec_bytecode` 的 monolithic `match` 替换为 `pc = table[op.tag()](self, op, pc)?`——每条 op 变为一次表查 + 一次间接调用，目的是给 CPU 间接跳转预测器提供 per-opcode 的独立预测状态（原 match 只有单一 jump 点）。handler 通过 `if let` 解构对应变体、不匹配路径落入冷 `unreachable!()`。稳定 Rust 无 sibling-tail-call，若 LLVM 未展开则按计划可退回 `match + #[inline(always)]` 保底。
  - 文件：`src/interp/bytecode.rs`、`src/interp/engine.rs`、`src/interp/handlers.rs`（新）
- **E3 解释器 LinearMul 快路径** · **[已实现]**：`src/interp/handlers.rs::exec_linear_mul` 对 factor ±1 列短路，跳过 `wrapping_mul` / `rem_euclid` 两条 ALU 指令（`delta = v as i32` / `delta = -(v as i32)`）；所有 factor 统一走 `Tape::add_at(off, delta)` 而非先前的 `move_ptr(off); add_current; move_ptr(-off)` 三联，少 1 次 grow 检查与 2 次 move-unit 统计更新。`src/runtime/tape.rs::add_at` 新增为共享入口，抽出 `ensure_range(target)` 给 `move_ptr` 和 `add_at` 复用；ptr_min / ptr_max / right_grew_bytes 仍正确记录，move_left_units / move_right_units 不再被 LinearMul 的虚拟访问虚增。通用 SIMD 展开（`rep stosb` / 切片 memcpy）留给 D2 / 未来的 interp SIMD。
  - 文件：`src/runtime/tape.rs`（新 `add_at` + 4 单测覆盖偏移/wrap/双向 grow）、`src/interp/handlers.rs`
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
  │    │    │    └→ B1 (DSE) ✓
  │    │    └→ B2 (zero-loop) ✓, B5 (LICM), B6 (unroll) ✓
  │    └→ B3 (LinearMul 泛化) ✓, B4 (pointer postponement) ✓, B7 (K6)
  │         │
  │         └→ C3 (displacement) ✓ → D2 / D4
  │
  └→ C2 (bounds batching) ✓ → C4 (scan hint) ✓、D2 (SIMD)

C1 (LIR peephole) ✓、D1 (指令选择) ✓、
D3 (buffered I/O — 解释器 ✓ / Linux 后端 ✓ / Windows 后端 pending)、
E1 / E2 (superinstruction + threaded dispatch) ✓、
E3 (interp LinearMul ±1 快路径) ✓、E4 (tape 倍增) ✓ 均可并行启动。
已落地：E5、C1、D1、E4、Phase A (A1–A4)、B1、B2、B3、B4、B6、C2、C3、C4、E1、E2、E3、D3 (解释器 + Linux 后端)、D5 (分支提示；对齐暂缓)。

Phase F 全部不在近期依赖图内。
```

---

## 4. 关键改动文件清单

| 层 | 文件 | 涉及阶段 |
|---|---|---|
| HIR | `src/ir/hir.rs` | B3 / B4 若需新变体 |
| HIR | `src/ir/optimize.rs` | Phase A / B 主修改点 |
| HIR | `src/ir/analysis/` | A1–A4 骨架（已落地） |
| HIR | `src/ir/dse.rs` | B1 DSE（已落地） |
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
