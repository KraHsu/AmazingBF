use std::collections::HashMap;
use super::ast::ScalarType;

#[derive(Debug, Clone)]
pub(crate) struct CellLayout {
    pub(crate) base: usize,
    pub(crate) width: usize,
    pub(crate) ty: ScalarType,
    pub(crate) array_len: Option<usize>,
}

impl CellLayout {
    pub(crate) fn is_array(&self) -> bool {
        self.array_len.is_some()
    }

    pub(crate) fn elem_width(&self) -> usize {
        self.ty.cell_width()
    }

    pub(crate) fn array_len(&self) -> usize {
        self.array_len.unwrap_or(1)
    }
}

#[derive(Debug)]
pub(crate) struct MemMap {
    pub(crate) vars: HashMap<String, CellLayout>,
    pub(crate) temp_base: usize,
}

impl MemMap {
    pub(crate) fn get(&self, name: &str) -> Option<&CellLayout> {
        self.vars.get(name)
    }
}

pub(crate) struct MemMapBuilder {
    vars: HashMap<String, CellLayout>,
    next_cell: usize,
}

impl MemMapBuilder {
    pub(crate) fn new() -> Self {
        MemMapBuilder { vars: HashMap::new(), next_cell: 0 }
    }

    pub(crate) fn alloc_scalar(&mut self, name: String, ty: ScalarType) {
        let width = ty.cell_width();
        self.vars.insert(name, CellLayout {
            base: self.next_cell,
            width,
            ty,
            array_len: None,
        });
        self.next_cell += width;
    }

    pub(crate) fn alloc_array(&mut self, name: String, ty: ScalarType, len: usize) {
        let elem_width = ty.cell_width();
        let total = elem_width * len;
        self.vars.insert(name, CellLayout {
            base: self.next_cell,
            width: total,
            ty,
            array_len: Some(len),
        });
        self.next_cell += total;
    }

    pub(crate) fn finalize(self) -> MemMap {
        MemMap {
            temp_base: self.next_cell,
            vars: self.vars,
        }
    }
}
