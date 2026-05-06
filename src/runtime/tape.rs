//! Bidirectional tape storage shared by interpreter and backend.
//!
//! The `Tape` stores all cells in one contiguous `Vec<u8>`, plus an
//! `origin` index that marks which slot corresponds to logical cell 0.
//! That keeps the interpreter's hot cell ops on a single slice instead of
//! branching across split left/right buffers. Growth still uses geometric
//! doubling: extending to the right is a plain `Vec::resize`, while the
//! colder left-growth path recentres the whole buffer with extra headroom.
//! All storage lives in safe `Vec<u8>`; `#![forbid(unsafe_code)]` rules out
//! `mmap`.
//!
//! `TapeStats` captures runtime usage (pointer range, growth, total
//! movement) so `--interp-debug` can summarize behaviour after a program
//! finishes. The plain interpreter fast path keeps this accounting off by
//! default; callers opt in only when they actually need the summary or the
//! tiered-JIT's flat-tape bridge.

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
/// Supports bidirectional growth: logical cell `0` lives at `cells[origin]`,
/// cells to the right occupy larger indices, and negative logical cells occupy
/// smaller indices. The current pointer is stored as an absolute index into the
/// same buffer, so current-cell reads/writes avoid per-access sign mapping.
///
/// Cells [−65536, −1] are reserved as the GUI screen framebuffer when the
/// interpreter runs in GUI mode (256×256 pixels, RGB332 encoding).
#[derive(Debug, Clone)]
pub(crate) struct Tape {
    cells: Vec<u8>,
    origin: usize,
    ptr_abs: usize,
    ptr_logical: isize,
    stats: Option<TapeStats>,
}

impl Tape {
    /// Create a fresh tape with `initial_len` cells on the right side (minimum 1).
    pub(crate) fn new(initial_len: usize) -> Self {
        Self::new_with_stats(initial_len, true)
    }

    /// Create a fresh tape without collecting movement / growth statistics.
    pub(crate) fn new_untracked(initial_len: usize) -> Self {
        Self::new_with_stats(initial_len, false)
    }

    fn new_with_stats(initial_len: usize, track_stats: bool) -> Self {
        let len = initial_len.max(1);
        Self {
            cells: vec![0; len],
            origin: 0,
            ptr_abs: 0,
            ptr_logical: 0,
            stats: track_stats.then_some(TapeStats {
                initial_len: len,
                final_len: len,
                ptr_min: 0,
                ptr_max: 0,
                right_grew_bytes: 0,
                move_left_units: 0,
                move_right_units: 0,
            }),
        }
    }

    /// Snapshot of tape usage statistics.
    pub(crate) fn stats(&self) -> &TapeStats {
        self.stats
            .as_ref()
            .expect("tape statistics are disabled for this tape")
    }

    /// Turn on tape statistics for future operations.
    pub(crate) fn enable_stats(&mut self) {
        if self.stats.is_some() {
            return;
        }
        let initial_len = self.right_len();
        let final_len = self.cells.len();
        let ptr = self.ptr_logical;
        self.stats = Some(TapeStats {
            initial_len,
            final_len,
            ptr_min: ptr,
            ptr_max: ptr,
            right_grew_bytes: 0,
            move_left_units: 0,
            move_right_units: 0,
        });
    }

    /// Returns the current tape pointer index (may be negative).
    #[inline(always)]
    pub(crate) fn ptr(&self) -> isize {
        self.ptr_logical
    }

    /// Returns the value of the current cell.
    #[inline(always)]
    pub(crate) fn current(&self) -> u8 {
        self.cells[self.ptr_abs]
    }

    /// Sets the value of the current cell.
    #[inline(always)]
    pub(crate) fn set_current(&mut self, value: u8) {
        self.cells[self.ptr_abs] = value;
    }

    /// Applies wrapping addition or subtraction to the current cell.
    #[inline(always)]
    pub(crate) fn add_current(&mut self, delta: i32) {
        let cell = &mut self.cells[self.ptr_abs];
        *cell = cell.wrapping_add(delta as u8);
    }

    /// Wrapping-add `delta` to the cell at `self.ptr + off`, growing the
    /// tape on demand if the target index is out of the current bounds.
    /// Does **not** move `self.ptr` and does **not** register move-unit
    /// stats for the virtual visit — meant for compound ops like
    /// `LinearMul` that touch several offsets without walking the
    /// pointer.  `ptr_min` / `ptr_max` / `right_grew_bytes` / `final_len`
    /// are updated so the tape-usage summary still reflects the cell
    /// the op actually read or wrote.
    #[inline(always)]
    pub(crate) fn add_at(&mut self, off: isize, delta: i32) {
        let target = self.offset_index(off);
        let cell = &mut self.cells[target];
        *cell = cell.wrapping_add(delta as u8);
    }

    /// Set the cell at `self.ptr + off` to `val`, growing the tape on
    /// demand.  Like [`add_at`](Self::add_at), does not move `self.ptr`
    /// and does not register move-unit stats.
    #[inline(always)]
    pub(crate) fn set_at(&mut self, off: isize, val: u8) {
        let target = self.offset_index(off);
        self.cells[target] = val;
    }

    #[inline(always)]
    fn right_len(&self) -> usize {
        self.cells.len() - self.origin
    }

    #[inline]
    fn track_target(&mut self, target: isize) {
        if let Some(stats) = self.stats.as_mut() {
            stats.ptr_min = stats.ptr_min.min(target);
            stats.ptr_max = stats.ptr_max.max(target);
        }
    }

    #[inline(always)]
    fn offset_index(&mut self, off: isize) -> usize {
        let target_logical = self.ptr_logical + off;
        if self.stats.is_some() {
            self.track_target(target_logical);
        }
        if let Some(target_abs) = self.ptr_abs.checked_add_signed(off) {
            if target_abs < self.cells.len() {
                return target_abs;
            }
        }
        self.grow_for_target(target_logical)
    }

    #[cold]
    fn grow_for_target(&mut self, target: isize) -> usize {
        if target >= 0 {
            let needed = target as usize + 1;
            let old_len = self.right_len();
            if needed > old_len {
                let new_len = needed.max(old_len.saturating_mul(2));
                self.cells.resize(self.origin + new_len, 0);
                if let Some(stats) = self.stats.as_mut() {
                    stats.right_grew_bytes += new_len - old_len;
                }
            }
        } else {
            let needed = (-target) as usize;
            let old_len = self.origin;
            if needed > old_len {
                let new_len = needed.max(old_len.saturating_mul(2)).max(8);
                let shift = new_len - old_len;
                let old_cells = self.cells.len();
                let right_len = self.right_len();
                let mut cells = vec![0; new_len + right_len];
                cells[shift..shift + old_cells].copy_from_slice(&self.cells);
                self.cells = cells;
                self.origin = new_len;
                self.ptr_abs += shift;
            }
        }

        if let Some(stats) = self.stats.as_mut() {
            stats.final_len = self.cells.len();
        }
        self.origin
            .checked_add_signed(target)
            .expect("target should be mapped after growth")
    }

    /// Moves the pointer by `delta`. Grows the tape automatically in both directions.
    #[inline(always)]
    pub(crate) fn move_ptr(&mut self, delta: isize) {
        let target_logical = self.ptr_logical + delta;
        if let Some(stats) = self.stats.as_mut() {
            match delta.cmp(&0) {
                std::cmp::Ordering::Less => stats.move_left_units += (-delta) as u64,
                std::cmp::Ordering::Greater => stats.move_right_units += delta as u64,
                std::cmp::Ordering::Equal => {}
            }
            stats.ptr_min = stats.ptr_min.min(target_logical);
            stats.ptr_max = stats.ptr_max.max(target_logical);
        }

        if let Some(target_abs) = self.ptr_abs.checked_add_signed(delta) {
            if target_abs < self.cells.len() {
                self.ptr_abs = target_abs;
                self.ptr_logical = target_logical;
                return;
            }
        }
        self.ptr_abs = self.grow_for_target(target_logical);
        self.ptr_logical = target_logical;
    }

    /// Minimum byte capacity (page-rounded) a flat buffer needs to hold
    /// the tape's currently visited span. Used by the tiered-JIT
    /// persistent-scratch path to size its mmap'd buffer.
    #[cfg(target_os = "linux")]
    pub(crate) fn flat_required_bytes(&self) -> usize {
        let stats = self
            .stats
            .as_ref()
            .expect("flat-tape snapshot requires tape statistics");
        let lo = stats.ptr_min;
        let hi = stats.ptr_max;
        let span = (hi - lo) as usize + 1;
        span.next_multiple_of(4096)
    }

    /// Snapshot the tape into a contiguous flat buffer suitable for JIT
    /// execution. Returns `(flat_buf, data_ptr_offset)` where
    /// `data_ptr_offset` is the byte offset of the current cell within
    /// `flat_buf`.
    ///
    /// The flat buffer covers `[ptr_min, ptr_max]` inclusive, zero-padded
    /// to at least `min_size` bytes and page-aligned for mmap compatibility.
    #[cfg(target_os = "linux")]
    #[allow(dead_code)] // reason: kept as the simpler one-shot API; the tiered JIT now uses snapshot_flat_into
    pub(crate) fn snapshot_flat(&self, min_size: usize) -> (Vec<u8>, usize) {
        let stats = self
            .stats
            .as_ref()
            .expect("flat-tape snapshot requires tape statistics");
        let lo = stats.ptr_min;
        let hi = stats.ptr_max;
        let span = (hi - lo) as usize + 1;
        let size = span.max(min_size).next_multiple_of(4096);
        let mut flat = vec![0u8; size];
        let data_ptr_offset = self.snapshot_flat_into(&mut flat);
        (flat, data_ptr_offset)
    }

    /// Snapshot the tape into a caller-provided flat buffer, returning the
    /// byte offset of the current cell within `flat`.
    ///
    /// The buffer must be at least [`Self::flat_required_bytes`] long; the
    /// region from byte `0` up to `right_start + right.len()` is zeroed
    /// then overwritten with the live cells, and the trailing region (if
    /// any) is zeroed too. Used by the tiered-JIT persistent-scratch path
    /// to avoid the `Vec<u8>` allocation that `snapshot_flat` does on every
    /// dispatch.
    #[cfg(target_os = "linux")]
    pub(crate) fn snapshot_flat_into(&self, flat: &mut [u8]) -> usize {
        let stats = self
            .stats
            .as_ref()
            .expect("flat-tape snapshot requires tape statistics");
        let lo = stats.ptr_min;
        let span = stats.visited_span();
        let start = self
            .origin
            .checked_add_signed(lo)
            .expect("visited span should stay mapped");

        flat.fill(0);
        flat[..span].copy_from_slice(&self.cells[start..start + span]);

        self.ptr_abs - start
    }

    /// Restore tape contents from a flat buffer produced by JIT execution.
    /// `flat` covers the same range as the last `snapshot_flat` call;
    /// `data_ptr_offset` is the JIT's final data pointer position within
    /// the flat buffer.
    #[cfg(target_os = "linux")]
    pub(crate) fn restore_from_flat(&mut self, flat: &[u8], data_ptr_offset: usize) {
        let stats = self
            .stats
            .as_ref()
            .expect("flat-tape restore requires tape statistics");
        let lo = stats.ptr_min;
        let span = stats.visited_span();
        let start = self
            .origin
            .checked_add_signed(lo)
            .expect("visited span should stay mapped");

        self.cells[start..start + span].copy_from_slice(&flat[..span]);
        self.ptr_abs = start + data_ptr_offset;
        self.ptr_logical = lo + data_ptr_offset as isize;
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

    #[test]
    fn untracked_tape_still_moves_and_grows() {
        let mut t = Tape::new_untracked(4);
        t.move_ptr(10);
        t.add_at(-13, 1);
        assert_eq!(t.ptr(), 10);
        t.move_ptr(-13);
        assert_eq!(t.current(), 1);
        t.enable_stats();
        let s = t.stats();
        assert_eq!(s.initial_len, 11);
        assert_eq!(s.final_len, 19);
        assert_eq!(s.ptr_min, -3);
        assert_eq!(s.ptr_max, -3);
    }

}
