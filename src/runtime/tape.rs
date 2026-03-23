/// The memory tape for runtime.
///
/// Current implementation details:
/// - the cell type is fixed to `u8`
/// - the initial length is configurable
/// - the tape grows automatically when the pointer moves to the right
/// - moving the pointer to the left of index 0 returns an error
#[derive(Debug, Clone)]
pub struct Tape {
    cells: Vec<u8>,
    ptr: usize,
}

#[derive(Debug)]
pub enum TapeError {
    PointerUnderflow,
}

impl Tape {
    pub fn new(initial_len: usize) -> Self {
        let len = initial_len.max(1);
        Self {
            cells: vec![0; len],
            ptr: 0,
        }
    }

    /// Returns the current pointer position.
    pub fn ptr(&self) -> usize {
        self.ptr
    }

    /// Returns the value of the current cell.
    pub fn current(&self) -> u8 {
        self.cells[self.ptr]
    }

    /// Sets the value of the current cell.
    pub fn set_current(&mut self, value: u8) {
        self.cells[self.ptr] = value;
    }

    /// Applies wrapping addition or subtraction to the current cell.
    pub fn add_current(&mut self, delta: i32) {
        let cur = self.cells[self.ptr];
        self.cells[self.ptr] = if delta >= 0 {
            cur.wrapping_add(delta as u8)
        } else {
            cur.wrapping_sub((-delta) as u8)
        };
    }

    /// Moves the pointer.
    pub fn move_ptr(&mut self, delta: isize) -> Result<(), TapeError> {
        let next = self.ptr as isize + delta;
        if next < 0 {
            return Err(TapeError::PointerUnderflow);
        }

        self.ptr = next as usize;

        if self.ptr >= self.cells.len() {
            self.cells.resize(self.ptr + 1, 0);
        }

        Ok(())
    }

    /// Returns the current cells
    pub fn cells(&self) -> &[u8] {
        &self.cells
    }
}
