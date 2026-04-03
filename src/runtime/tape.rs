use thiserror::Error;

/// Statistics collected while a [`Tape`] is in use (pointer range, growth, move totals).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TapeStats {
    /// Length allocated at construction.
    pub initial_len: usize,
    /// Current backing store length (after any growth).
    pub final_len: usize,
    /// Smallest index the pointer visited.
    pub ptr_min: usize,
    /// Largest index the pointer visited.
    pub ptr_max: usize,
    /// Cells added by automatic growth beyond `initial_len`.
    pub right_growth: usize,
    /// Sum of absolute pointer deltas when moving left (`<` / negative HIR `Move`).
    pub move_left_units: u64,
    /// Sum of pointer deltas when moving right (`>` / positive HIR `Move`).
    pub move_right_units: u64,
}

impl TapeStats {
    /// Number of indices covered from the leftmost to rightmost visit (inclusive).
    pub fn visited_span(&self) -> usize {
        self.ptr_max.saturating_sub(self.ptr_min).saturating_add(1)
    }
}

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
    stats: TapeStats,
}

#[derive(Debug, Error)]
pub enum TapeError {
    #[error("pointer underflow at {pos}")]
    PointerUnderflow { pos: isize },
}

impl Tape {
    pub fn new(initial_len: usize) -> Self {
        let len = initial_len.max(1);
        Self {
            cells: vec![0; len],
            ptr: 0,
            stats: TapeStats {
                initial_len: len,
                final_len: len,
                ptr_min: 0,
                ptr_max: 0,
                right_growth: 0,
                move_left_units: 0,
                move_right_units: 0,
            },
        }
    }

    /// Snapshot of tape usage statistics (updated by pointer moves and growth).
    pub fn stats(&self) -> &TapeStats {
        &self.stats
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
            return Err(TapeError::PointerUnderflow { pos: next });
        }

        if delta < 0 {
            self.stats.move_left_units += (-delta) as u64;
        } else if delta > 0 {
            self.stats.move_right_units += delta as u64;
        }

        self.ptr = next as usize;
        self.stats.ptr_min = self.stats.ptr_min.min(self.ptr);
        self.stats.ptr_max = self.stats.ptr_max.max(self.ptr);

        if self.ptr >= self.cells.len() {
            let old_len = self.cells.len();
            self.cells.resize(self.ptr + 1, 0);
            let grown = self.cells.len() - old_len;
            self.stats.right_growth += grown;
        }
        self.stats.final_len = self.cells.len();

        Ok(())
    }

    /// Returns the current cells
    pub fn cells(&self) -> &[u8] {
        &self.cells
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_track_right_growth_and_span() {
        let mut t = Tape::new(4);
        t.move_ptr(10).unwrap();
        let s = t.stats();
        assert_eq!(s.initial_len, 4);
        assert_eq!(s.final_len, 11);
        assert_eq!(s.ptr_min, 0);
        assert_eq!(s.ptr_max, 10);
        assert_eq!(s.right_growth, 7);
        assert_eq!(s.move_right_units, 10);
        assert_eq!(s.move_left_units, 0);
        assert_eq!(s.visited_span(), 11);
    }

    #[test]
    fn stats_track_left_moves_and_min_ptr() {
        let mut t = Tape::new(8);
        t.move_ptr(3).unwrap();
        t.move_ptr(-2).unwrap();
        let s = t.stats();
        assert_eq!(s.ptr_min, 0);
        assert_eq!(s.ptr_max, 3);
        assert_eq!(s.move_right_units, 3);
        assert_eq!(s.move_left_units, 2);
        assert_eq!(s.visited_span(), 4);
    }

    #[test]
    fn failed_left_move_does_not_update_move_stats() {
        let mut t = Tape::new(2);
        assert!(t.move_ptr(-1).is_err());
        let s = t.stats();
        assert_eq!(s.move_left_units, 0);
        assert_eq!(s.ptr_min, 0);
        assert_eq!(s.ptr_max, 0);
    }
}
