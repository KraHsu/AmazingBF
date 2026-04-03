//! # 后端模块 (backend/mod.rs)
//!
//! 本模块组织了编译器后端的所有子模块：
//!
//! - `asm`: 汇编 IR 定义（`AsmInst`, `AsmProgram` 等）
//! - `codegen`: LIR → 汇编 IR 的代码生成器
//! - `x86_64`: x86_64 目标平台（编码器 + ELF 生成器）

pub mod asm;
pub mod codegen;
pub mod x86_64;
