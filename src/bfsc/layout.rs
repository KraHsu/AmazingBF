//! Memory layout used by BFS codegen.
//!
//! Variables and arrays are packed into consecutive BF tape cells by
//! `MemMapBuilder`; the resulting `MemMap` records each declaration's base
//! address, element width, and array length. Codegen uses this to translate
//! BFS references into concrete tape offsets and transient scratch cells.

use super::ast::ScalarType;
use std::collections::HashMap;

/// Storage strategy for an array binding.
///
/// Drives `bfsc::codegen::arr_read` / `arr_write`: linear scan emits
/// O(arr_len^2) BF and is fine for tiny arrays, while `Walk` uses a
/// moving-pointer idiom that emits O(1) BF per access at the cost of
/// 4 extra scratch cells per element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArrayLayout {
    /// `arr_len * elem_width` contiguous cells. The legacy 1- and 2-byte
    /// linear-scan code paths in codegen.rs operate on this shape.
    Linear,
    /// Moving-pointer ("walk") storage. Each element occupies 4 cells
    /// (V, S_a control, S_b survivor, S_c value-carrier). `chunk_size`
    /// elements live consecutively in `chunk_size` slots. The chunk's
    /// envelope is:
    ///   [P_a, P_b, P_c, V_0, S_0a, S_0b, S_0c, …, V_(N-1), S_(N-1)a,
    ///    S_(N-1)b, S_(N-1)c, E, P_d]
    /// `4*chunk_size + 5` cells. The 3 leading prefix cells are the
    /// outer walk's macro counter / survivor / payload; the trailing E
    /// (offset `4*N+3`) parks the chunk-id survivor across the inner
    /// walk; and P_d (offset `4*N+4`) carries the write value alongside
    /// the macro walk on write paths. Read paths leave P_d untouched.
    /// Only valid for `ty == U8` (the walk idiom is single-byte
    /// counter-based).
    Walk {
        chunk_size: usize,
        num_chunks: usize,
    },
}

impl ArrayLayout {
    /// Total cell count for this array layout.
    pub(crate) fn total_cells(&self, array_len: usize, elem_width: usize) -> usize {
        match self {
            ArrayLayout::Linear => array_len * elem_width,
            ArrayLayout::Walk {
                chunk_size,
                num_chunks,
            } => num_chunks * (4 * chunk_size + 5),
        }
    }
}

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
    /// Storage strategy. `Linear` for scalars and the legacy array path;
    /// `Walk` for arrays that opted into the moving-pointer idiom.
    pub(crate) layout: ArrayLayout,
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
                layout: ArrayLayout::Linear,
            },
        );
        self.next_cell += width;
    }

    /// Allocate cells for an array binding and advance the cursor past it.
    ///
    /// Picks the storage strategy automatically: linear for arrays small
    /// enough to keep the legacy linear-scan emit at a tolerable size
    /// (≤ 256 cells with the existing 1-byte path), or walk-based with
    /// 256-element chunks once the linear path's quadratic emit explodes.
    pub(crate) fn alloc_array(&mut self, name: String, ty: ScalarType, len: usize) {
        // Walk storage is single-byte counter only — restrict to u8 arrays.
        // The threshold mirrors the old 1-byte index path: arrays up to 256
        // elements stay byte-identical to the previous emitter; anything
        // larger gets the walk layout (with chunked dispatch when arr_len
        // exceeds the chunk size of 256).
        let layout = if matches!(ty, ScalarType::U8) && len > 256 {
            const CHUNK: usize = 256;
            let num_chunks = len.div_ceil(CHUNK);
            ArrayLayout::Walk {
                chunk_size: CHUNK,
                num_chunks,
            }
        } else {
            ArrayLayout::Linear
        };
        let elem_width = ty.cell_width();
        let total = layout.total_cells(len, elem_width);
        self.vars.insert(
            name,
            CellLayout {
                base: self.next_cell,
                width: total,
                ty,
                array_len: Some(len),
                layout,
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
