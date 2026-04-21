//! Intermediate representations and lowering / optimization passes.

pub(crate) mod analysis;
pub(crate) mod dse;
pub(crate) mod hir;
pub(crate) mod lir;
pub(crate) mod lir_opt;
pub(crate) mod lower;
pub(crate) mod optimize;
