//! HIR analysis infrastructure (`Phase A` of the optimization plan).
//!
//! Provides reusable, side-effect-free queries over HIR fragments that later
//! HIR / LIR passes can consume. Nothing in this module mutates HIR; every
//! analysis returns a value that a driver pass can inspect.

pub(crate) mod dataflow;
pub(crate) mod lattice;
pub(crate) mod loop_effect;
pub(crate) mod tape_state;
