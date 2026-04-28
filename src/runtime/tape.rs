//! Bidirectional tape storage shared by interpreter and backend.
//!
//! The `Tape` backs both positive and negative cell indices by splitting
//! storage into two `Vec<u8>` halves. Each half grows via geometric
//! doubling on out-of-range pointer movement — when the pointer escapes
//! the currently allocated side, the side is resized to
//! `max(needed, old_len * 2)` (with a small lower bound on the left side
//! whose initial length is zero). Doubling amortises allocation cost to
//! O(1) per cell touched and keeps cadence independent of the pointer's
//! step size, while exactly-needed resizing would degrade to O(n) for
//! single-step walks past the boundary. All storage lives in safe
//! `Vec<u8>`; `#![forbid(unsafe_code)]` rules out `mmap`.
//!
//! `TapeStats` captures runtime usage (pointer range, growth, total
//! movement) so `--interp-debug` can summarize behaviour after a program
//! finishes.

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
    /// Bytes appended to the right side beyond `initial_len` across all
    /// doubling events. Measures allocated capacity growth, not the
    /// highest cell visited.
    pub(crate) right_grew_bytes: usize,
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
                right_grew_bytes: 0,
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

    /// Wrapping-add `delta` to the cell at `self.ptr + off`, growing the
    /// tape on demand if the target index is out of the current bounds.
    /// Does **not** move `self.ptr` and does **not** register move-unit
    /// stats for the virtual visit — meant for compound ops like
    /// `LinearMul` that touch several offsets without walking the
    /// pointer.  `ptr_min` / `ptr_max` / `right_grew_bytes` / `final_len`
    /// are updated so the tape-usage summary still reflects the cell
    /// the op actually read or wrote.
    pub(crate) fn add_at(&mut self, off: isize, delta: i32) {
        let target = self.ptr + off;
        self.ensure_range(target);
        let cell = if target >= 0 {
            &mut self.right[target as usize]
        } else {
            &mut self.left[(-target - 1) as usize]
        };
        *cell = if delta >= 0 {
            cell.wrapping_add(delta as u8)
        } else {
            cell.wrapping_sub((-delta) as u8)
        };
    }

    /// Set the cell at `self.ptr + off` to `val`, growing the tape on
    /// demand.  Like [`add_at`](Self::add_at), does not move `self.ptr`
    /// and does not register move-unit stats.
    pub(crate) fn set_at(&mut self, off: isize, val: u8) {
        let target = self.ptr + off;
        self.ensure_range(target);
        let cell = if target >= 0 {
            &mut self.right[target as usize]
        } else {
            &mut self.left[(-target - 1) as usize]
        };
        *cell = val;
    }

    /// Shared tape-growth helper for `move_ptr` / `add_at`.  Grows the
    /// appropriate side geometrically (with the left-side-8 floor) and
    /// updates `ptr_min` / `ptr_max` / `right_grew_bytes` / `final_len`.
    fn ensure_range(&mut self, target: isize) {
        self.stats.ptr_min = self.stats.ptr_min.min(target);
        self.stats.ptr_max = self.stats.ptr_max.max(target);

        if target >= 0 {
            let needed = target as usize + 1;
            if needed > self.right.len() {
                let old_len = self.right.len();
                let new_len = needed.max(old_len.saturating_mul(2));
                self.right.resize(new_len, 0);
                self.stats.right_grew_bytes += new_len - old_len;
            }
        } else {
            let needed = (-target) as usize;
            if needed > self.left.len() {
                let old_len = self.left.len();
                let new_len = needed.max(old_len.saturating_mul(2)).max(8);
                self.left.resize(new_len, 0);
            }
        }

        self.stats.final_len = self.right.len() + self.left.len();
    }

    /// Moves the pointer by `delta`. Grows the tape automatically in both directions.
    pub(crate) fn move_ptr(&mut self, delta: isize) {
        match delta.cmp(&0) {
            std::cmp::Ordering::Less => self.stats.move_left_units += (-delta) as u64,
            std::cmp::Ordering::Greater => self.stats.move_right_units += delta as u64,
            std::cmp::Ordering::Equal => {}
        }

        self.ptr += delta;
        self.ensure_range(self.ptr);
    }

    /// Snapshot the tape into a contiguous flat buffer suitable for JIT
    /// execution. Returns `(flat_buf, data_ptr_offset)` where
    /// `data_ptr_offset` is the byte offset of the current cell within
    /// `flat_buf`.
    ///
    /// The flat buffer covers `[ptr_min, ptr_max]` inclusive, zero-padded
    /// to at least `min_size` bytes and page-aligned for mmap compatibility.
    #[cfg(target_os = "linux")]
    pub(crate) fn snapshot_flat(&self, min_size: usize) -> (Vec<u8>, usize) {
        let lo = self.stats.ptr_min;
        let hi = self.stats.ptr_max;
        let span = (hi - lo) as usize + 1;
        let size = span.max(min_size).next_multiple_of(4096);
        let mut flat = vec![0u8; size];

        let right_start = (-lo) as usize;
        let right_copy = self.right.len().min(size - right_start);
        flat[right_start..right_start + right_copy].copy_from_slice(&self.right[..right_copy]);

        for (i, &val) in self.left.iter().enumerate() {
            let idx = (-lo) as usize - (i + 1);
            if idx < size {
                flat[idx] = val;
            }
        }

        let data_ptr_offset = (self.ptr - lo) as usize;
        (flat, data_ptr_offset)
    }

    /// Restore tape contents from a flat buffer produced by JIT execution.
    /// `flat` covers the same range as the last `snapshot_flat` call;
    /// `data_ptr_offset` is the JIT's final data pointer position within
    /// the flat buffer.
    #[cfg(target_os = "linux")]
    pub(crate) fn restore_from_flat(&mut self, flat: &[u8], data_ptr_offset: usize) {
        let lo = self.stats.ptr_min;

        let right_start = (-lo) as usize;
        let right_copy = self.right.len().min(flat.len() - right_start);
        self.right[..right_copy].copy_from_slice(&flat[right_start..right_start + right_copy]);

        for (i, val) in self.left.iter_mut().enumerate() {
            let idx = (-lo) as usize - (i + 1);
            if idx < flat.len() {
                *val = flat[idx];
            }
        }

        self.ptr = lo + data_ptr_offset as isize;
        self.ensure_range(self.ptr);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_track_right_growth_and_span() {
        // needed = 11 exceeds `old_len * 2 = 8`, so the doubling rule falls
        // back to the exact needed size.
        let mut t = Tape::new(4);
        t.move_ptr(10);
        let s = t.stats();
        assert_eq!(s.initial_len, 4);
        assert_eq!(s.final_len, 11);
        assert_eq!(s.ptr_min, 0);
        assert_eq!(s.ptr_max, 10);
        assert_eq!(s.right_grew_bytes, 7);
        assert_eq!(s.move_right_units, 10);
        assert_eq!(s.move_left_units, 0);
        assert_eq!(s.visited_span(), 11);
    }

    #[test]
    fn doubling_overshoots_needed_on_small_move() {
        // needed = 6 fits inside `old_len * 2 = 8`, so the resize lands on 8
        // rather than the exact needed size — the point of geometric growth.
        let mut t = Tape::new(4);
        t.move_ptr(5);
        let s = t.stats();
        assert_eq!(s.final_len, 8);
        assert_eq!(s.right_grew_bytes, 4);
        assert_eq!(s.ptr_max, 5);
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

    #[test]
    fn add_at_targets_offset_without_moving_ptr() {
        // add_at(3, 7) writes cell[3] but leaves ptr at 0.
        let mut t = Tape::new(8);
        t.add_at(3, 7);
        assert_eq!(t.ptr(), 0);
        t.move_ptr(3);
        assert_eq!(t.current(), 7);
        t.move_ptr(-3);
        // No move-unit stats from the add_at itself; only the two
        // inspection `move_ptr` calls count.
        let s = t.stats();
        assert_eq!(s.move_right_units, 3);
        assert_eq!(s.move_left_units, 3);
    }

    #[test]
    fn add_at_wraps_byte_on_overflow() {
        let mut t = Tape::new(4);
        t.set_current(250);
        t.add_at(0, 10); // 250 + 10 = 260 ≡ 4 (mod 256)
        assert_eq!(t.current(), 4);
        t.add_at(0, -5); // 4 - 5 ≡ 255
        assert_eq!(t.current(), 255);
    }

    #[test]
    fn add_at_grows_tape_on_out_of_range_offset() {
        let mut t = Tape::new(4);
        assert_eq!(t.stats().final_len, 4);
        t.add_at(10, 1);
        // Grows right side to cover offset 10; ptr stays at 0.
        let s = t.stats();
        assert!(s.final_len >= 11);
        assert_eq!(s.ptr_max, 10);
        assert_eq!(t.ptr(), 0);
        assert_eq!(s.move_right_units, 0);
    }

    #[test]
    fn add_at_grows_left_side_for_negative_offset() {
        let mut t = Tape::new(4);
        t.add_at(-3, 42);
        assert_eq!(t.ptr(), 0);
        t.move_ptr(-3);
        assert_eq!(t.current(), 42);
        let s = t.stats();
        assert_eq!(s.ptr_min, -3);
    }

    #[test]
    fn set_at_writes_without_moving_ptr() {
        let mut t = Tape::new(8);
        t.set_at(5, 99);
        assert_eq!(t.ptr(), 0);
        t.move_ptr(5);
        assert_eq!(t.current(), 99);
    }

    #[test]
    fn set_at_negative_offset() {
        let mut t = Tape::new(4);
        t.set_at(-2, 77);
        assert_eq!(t.ptr(), 0);
        t.move_ptr(-2);
        assert_eq!(t.current(), 77);
        assert_eq!(t.stats().ptr_min, -2);
    }

    #[test]
    fn set_at_grows_tape_on_demand() {
        let mut t = Tape::new(4);
        t.set_at(20, 1);
        assert!(t.stats().final_len >= 21);
        assert_eq!(t.stats().ptr_max, 20);
        assert_eq!(t.ptr(), 0);
    }
}
