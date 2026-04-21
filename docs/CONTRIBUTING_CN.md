# 贡献指南

## 开发基线

- 使用 Rust `1.94` 或更新版本（与 `Cargo.toml` 中的 `rust-version` 一致）。
- 提交更改前运行 `cargo fmt`。
- 运行 `cargo clippy --all-targets -- -D warnings` 进行代码规范检查。
- 运行 `cargo test` 执行回归测试。
- 运行 `cargo bench --bench compile_levels` 跑 `tests/cases/*.bf` 在 `-O0..3` 下的「编译 + 运行」耗时总表。
- 运行 `cargo bench --bench standard_suite` 跑 matslina BF 套件在各优化级别下的解释/执行耗时（支持 Criterion 的 `--save-baseline` / `--baseline`）。

## 架构规则

- 保持核心流水线一致：source → lexer → parser → AST → HIR → optimize → LIR。
- `interpret` 模式在优化后的 HIR 处停止，并在 `src/interp/engine.rs` 中执行。
- 只有 `dump` 和 `compile` 模式会继续进入 LIR / 后端汇编阶段。
- `compile` 模式必须确保 Linux ELF 和 Windows PE64 的输出行为与 `README.md` 和 `man/amazingbf.1` 保持一致。

## 代码风格

- 实现细节优先使用 `pub(crate)`；只有在确实需要稳定的公共 API 时才扩大可见性。
- 使用模块文档说明职责和不变量，尤其是围绕 IR、后端和运行时代码的部分。
- 函数注释应简洁且基于事实。说明「为什么」或关键不变量，避免描述显而易见的赋值过程。
- 除非更改明确针对 CLI 用户体验，否则应保持现有的用户可见 CLI 行为不变。

## 测试

- `tests/cases_pipeline.rs` 覆盖解释器和编译器输出的端到端测试（基于测试用例）。
- `tests/windows_target.rs` 覆盖 PE64 布局和跨目标行为。
- `tests/compile_artifacts.rs` 在 O0–O3 下校验 ELF/PE 产物、`.asm`/`.lst` 输出以及 EOF 语义。
- `tests/cases/*.bf` 为测试用例文件，而非 Rust 源文件；需保持 `.bf`、`.in`、`.out` 和 `.md` 的命名一致。

## 基准测试

- `benches/compile_levels.rs` 负责 `tests/cases/*.bf` 的编译 + 运行耗时总表（自定义 harness，不依赖 Criterion）。
- `benches/standard_suite.rs` 负责 matslina BF 程序的解释/执行耗时（基于 Criterion）。
- 两者均为开发者本地运行；CI 仅执行 `cargo test`。

## 文档

- 当架构、CLI 行为、输出或工作流发生变化时，更新 `README.md`，并同步 `docs/README_CN.md`。
- 当 CLI 帮助或环境变量发生变化时，更新 `man/amazingbf.1`。
- 当流水线或后端相关事实发生变化时，更新 `.cursor/rules/project-architecture.mdc`。
- 贡献约定变更时，同步更新 `CONTRIBUTING.md` 与 `docs/CONTRIBUTING_CN.md`。
