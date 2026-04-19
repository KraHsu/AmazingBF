//! Bidirectional tape storage shared by interpreter and backend.
//!
//! The `Tape` backs both positive and negative cell indices by splitting
//! storage into two `Vec<u8>` halves that grow on demand. `TapeStats` captures
//! runtime usage (pointer range, growth, total movement) so `--interp-debug`
//! can summarize behaviour after a program finishes.

/// Statistics collected while a [`Tape`] is in use (pointer range, growth, move totals).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TapeStats {
    /// Length allocated at construction (right side only).
    pub(crate) initial_len: usize,
    /// Total backing store length after growth (right.len() + left.len()).
    pub(crate) final_len: usize,
    /// Smallest tape index the pointer visited.
    pub(crate) ptr_min: isize,
    /// Largest tape index the pointer visited.
    pub(crate) ptr_max: isize,
    /// Cells added to the right side beyond `initial_len`.
    pub(crate) right_growth: usize,
    /// Sum of absolute pointer deltas when moving left (`<` / negative HIR `Move`).
    pub(crate) move_left_units: u64,
    /// Sum of pointer deltas when moving right (`>` / positive HIR `Move`).
    pub(crate) move_right_units: u64,
}

impl TapeStats {
    /// Number of indices covered from the leftmost to rightmost visit (inclusive).
    pub(crate) fn visited_span(&self) -> usize {
        (self.ptr_max - self.ptr_min) as usize + 1
    }
}

/// The memory tape for runtime.
///
/// Supports bidirectional growth: indices ≥ 0 are stored in `right`; indices < 0
/// are stored in `left` where `left[0]` = cell −1, `left[1]` = cell −2, etc.
/// Both sides grow automatically on demand, so the tape never returns an error on
/// pointer movement.
///
/// Cells [−65536, −1] are reserved as the GUI screen framebuffer when the
/// interpreter runs in GUI mode (256×256 pixels, RGB332 encoding).
#[derive(Debug, Clone)]
pub(crate) struct Tape {
    right: Vec<u8>, // cells[0], cells[1], ...
    left: Vec<u8>,  // cells[-1], cells[-2], ... (left[i] = cell[-(i+1)])
    ptr: isize,
    stats: TapeStats,
}

impl Tape {
    /// Create a fresh tape with `initial_len` cells on the right side (minimum 1).
    pub(crate) fn new(initial_len: usize) -> Self {
        let len = initial_len.max(1);
        Self {
            right: vec![0; len],
            left: Vec::new(),
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

    /// Snapshot of tape usage statistics.
    pub(crate) fn stats(&self) -> &TapeStats {
        &self.stats
    }

    /// Returns the current tape pointer index (may be negative).
    pub(crate) fn ptr(&self) -> isize {
        self.ptr
    }

    #[inline]
    fn cell(&self) -> &u8 {
        if self.ptr >= 0 {
            &self.right[self.ptr as usize]
        } else {
            &self.left[(-self.ptr - 1) as usize]
        }
    }

    #[inline]
    fn cell_mut(&mut self) -> &mut u8 {
        if self.ptr >= 0 {
            &mut self.right[self.ptr as usize]
        } else {
            &mut self.left[(-self.ptr - 1) as usize]
        }
    }

    /// Returns the value of the current cell.
    pub(crate) fn current(&self) -> u8 {
        *self.cell()
    }

    /// Sets the value of the current cell.
    pub(crate) fn set_current(&mut self, value: u8) {
        *self.cell_mut() = value;
    }

    /// Applies wrapping addition or subtraction to the current cell.
    pub(crate) fn add_current(&mut self, delta: i32) {
        let c = self.cell_mut();
        *c = if delta >= 0 {
            c.wrapping_add(delta as u8)
        } else {
            c.wrapping_sub((-delta) as u8)
        };
    }

    /// Moves the pointer by `delta`. Grows the tape automatically in both directions.
    pub(crate) fn move_ptr(&mut self, delta: isize) {
        match delta.cmp(&0) {
            std::cmp::Ordering::Less => self.stats.move_left_units += (-delta) as u64,
            std::cmp::Ordering::Greater => self.stats.move_right_units += delta as u64,
            std::cmp::Ordering::Equal => {}
        }

        self.ptr += delta;
        self.stats.ptr_min = self.stats.ptr_min.min(self.ptr);
        self.stats.ptr_max = self.stats.ptr_max.max(self.ptr);

        if self.ptr >= 0 {
            let needed = self.ptr as usize + 1;
            if needed > self.right.len() {
                let old_len = self.right.len();
                self.right.resize(needed, 0);
                self.stats.right_growth += self.right.len() - old_len;
            }
        } else {
            let needed = (-self.ptr) as usize;
            if needed > self.left.len() {
                self.left.resize(needed, 0);
            }
        }

        self.stats.final_len = self.right.len() + self.left.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_track_right_growth_and_span() {
        let mut t = Tape::new(4);
        t.move_ptr(10);
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
        t.move_ptr(3);
        t.move_ptr(-2);
        let s = t.stats();
        assert_eq!(s.ptr_min, 0);
        assert_eq!(s.ptr_max, 3);
        assert_eq!(s.move_right_units, 3);
        assert_eq!(s.move_left_units, 2);
        assert_eq!(s.visited_span(), 4);
    }

    #[test]
    fn tape_supports_negative_indices() {
        let mut t = Tape::new(4);
        t.move_ptr(-1);
        t.set_current(42);
        assert_eq!(t.current(), 42);
        // Moving back to 0 gives the original cell
        t.move_ptr(1);
        assert_eq!(t.current(), 0);
        let s = t.stats();
        assert_eq!(s.ptr_min, -1);
        assert_eq!(s.ptr_max, 0);
        assert_eq!(s.move_left_units, 1);
        assert_eq!(s.move_right_units, 1);
    }

    #[test]
    fn tape_screen_buffer_range() {
        let mut t = Tape::new(4);
        t.move_ptr(-65536);
        t.set_current(0xFF);
        assert_eq!(t.current(), 0xFF);
        let s = t.stats();
        assert_eq!(s.ptr_min, -65536);
        assert_eq!(s.ptr_max, 0);
        assert_eq!(s.visited_span(), 65537);
    }
}
