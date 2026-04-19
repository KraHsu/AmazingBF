//! Memory layout used by BFS codegen.
//!
//! Variables and arrays are packed into consecutive BF tape cells by
//! `MemMapBuilder`; the resulting `MemMap` records each declaration's base
//! address, element width, and array length. Codegen uses this to translate
//! BFS references into concrete tape offsets and transient scratch cells.

use super::ast::ScalarType;
use std::collections::HashMap;

/// Memory layout of one declared variable or array in the BFS source.
#[derive(Debug, Clone)]
pub(crate) struct CellLayout {
    /// Starting tape cell of this binding.
    pub(crate) base: usize,
    /// Total byte width (`elem_width * array_len` for arrays, `elem_width` for scalars).
    pub(crate) width: usize,
    /// Element scalar type (`u8`/`i16`/...).
    pub(crate) ty: ScalarType,
    /// Array element count; `None` for scalar bindings.
    pub(crate) array_len: Option<usize>,
}

impl CellLayout {
    /// Width in bytes of a single element.
    pub(crate) fn elem_width(&self) -> usize {
        self.ty.cell_width()
    }

    /// Element count; returns `1` for scalars so call sites can treat everything uniformly.
    pub(crate) fn array_len(&self) -> usize {
        self.array_len.unwrap_or(1)
    }
}

/// Finalised variable-to-cell layout consumed by `bfsc::codegen`.
#[derive(Debug)]
pub(crate) struct MemMap {
    /// Named bindings (variables and arrays).
    pub(crate) vars: HashMap<String, CellLayout>,
    /// First cell index available for transient scratch allocations.
    pub(crate) temp_base: usize,
}

impl MemMap {
    /// Look up the layout of the named binding, if any.
    pub(crate) fn get(&self, name: &str) -> Option<&CellLayout> {
        self.vars.get(name)
    }
}

/// Incremental builder that packs BFS declarations into consecutive tape cells.
pub(crate) struct MemMapBuilder {
    vars: HashMap<String, CellLayout>,
    next_cell: usize,
}

impl MemMapBuilder {
    /// Create an empty layout builder starting at cell 0.
    pub(crate) fn new() -> Self {
        MemMapBuilder {
            vars: HashMap::new(),
            next_cell: 0,
        }
    }

    /// Allocate cells for a scalar binding and advance the cursor past it.
    pub(crate) fn alloc_scalar(&mut self, name: String, ty: ScalarType) {
        let width = ty.cell_width();
        self.vars.insert(
            name,
            CellLayout {
                base: self.next_cell,
                width,
                ty,
                array_len: None,
            },
        );
        self.next_cell += width;
    }

    /// Allocate cells for an array binding and advance the cursor past it.
    pub(crate) fn alloc_array(&mut self, name: String, ty: ScalarType, len: usize) {
        let elem_width = ty.cell_width();
        let total = elem_width * len;
        self.vars.insert(
            name,
            CellLayout {
                base: self.next_cell,
                width: total,
                ty,
                array_len: Some(len),
            },
        );
        self.next_cell += total;
    }

    /// Consume the builder and produce a [`MemMap`] with `temp_base` set to the cursor.
    pub(crate) fn finalize(self) -> MemMap {
        MemMap {
            temp_base: self.next_cell,
            vars: self.vars,
        }
    }
}
