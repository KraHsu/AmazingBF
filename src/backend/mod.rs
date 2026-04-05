//! Backend stages after HIR lowering.
//!
//! This layer is entered only by `dump` and `compile` flows. `interpret` stops at
//! optimized HIR and executes in `src/interp/engine.rs`.
//!
//! Current backend structure:
//! - `asm`: backend-local assembly IR (`AsmProgram`, `AsmInst`)
//! - `codegen`: lower LIR into assembly IR
//! - `x86_64`: encode assembly IR and package it as either Linux ELF or Windows PE64

pub(crate) mod asm;
pub(crate) mod codegen;
pub(crate) mod x86_64;
