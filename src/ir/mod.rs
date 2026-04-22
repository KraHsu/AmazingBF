//! Intermediate representations and lowering / optimization passes.

pub(crate) mod analysis;
pub(crate) mod dse;
pub(crate) mod hir;
pub(crate) mod lir;
pub(crate) mod lir_opt;
pub(crate) mod lir_postpone;
pub(crate) mod lir_scan_hint;
pub(crate) mod lower;
pub(crate) mod optimize;
