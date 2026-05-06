//! Loop trip-count profiling for the interpreter.
//!
//! When enabled, the interpreter increments a counter every time a loop body
//! executes. For generic loops this still happens on the `LoopEnd` back-edge;
//! for specialized `LoopBlock` bytecode the fast-path body records one count
//! per iteration directly. After execution, the profile reports which loop
//! entry PCs exceeded a configurable hot threshold.
//!
//! The profiler is zero-cost when disabled: the interpreter simply doesn't
//! allocate or update the counters.

/// Per-loop trip count, keyed by the `LoopStart` pc index.
#[derive(Debug, Clone)]
pub(crate) struct LoopProfile {
    /// Trip counts indexed by `LoopStart` pc. Only loops that have been
    /// entered at least once have a non-zero entry.
    counts: Vec<u64>,
    /// Threshold above which a loop is considered "hot".
    threshold: u64,
}

impl LoopProfile {
    /// Create a new profile with `num_ops` slots (one per bytecode op).
    pub(crate) fn new(num_ops: usize, threshold: u64) -> Self {
        Self {
            counts: vec![0; num_ops],
            threshold,
        }
    }

    /// Record one loop-body execution for the loop whose entry opcode is at
    /// `start_pc`.
    #[inline]
    pub(crate) fn record_loop_iter(&mut self, start_pc: u32) {
        self.counts[start_pc as usize] += 1;
    }

    /// Returns the trip count for the loop at `start_pc`.
    #[inline]
    pub(crate) fn trip_count(&self, start_pc: u32) -> u64 {
        self.counts[start_pc as usize]
    }

    /// Returns an iterator over `(start_pc, trip_count)` for all loops that
    /// exceeded the hot threshold.
    #[allow(dead_code)] // reason: kept as a public surface for measurement-only callers; H3 uses trip_count+threshold inline
    pub(crate) fn hot_loops(&self) -> impl Iterator<Item = (u32, u64)> + '_ {
        self.counts
            .iter()
            .enumerate()
            .filter(move |&(_, &count)| count >= self.threshold)
            .map(|(pc, &count)| (pc as u32, count))
    }

    /// The configured hot threshold.
    pub(crate) fn threshold(&self) -> u64 {
        self.threshold
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_profile_has_zero_counts() {
        let p = LoopProfile::new(10, 100);
        for i in 0..10 {
            assert_eq!(p.trip_count(i), 0);
        }
    }

    #[test]
    fn record_increments_correct_slot() {
        let mut p = LoopProfile::new(5, 10);
        p.record_loop_iter(2);
        p.record_loop_iter(2);
        p.record_loop_iter(4);
        assert_eq!(p.trip_count(0), 0);
        assert_eq!(p.trip_count(2), 2);
        assert_eq!(p.trip_count(4), 1);
    }

    #[test]
    fn hot_loops_filters_by_threshold() {
        let mut p = LoopProfile::new(4, 3);
        for _ in 0..5 {
            p.record_loop_iter(1);
        }
        for _ in 0..2 {
            p.record_loop_iter(3);
        }
        let hot: Vec<_> = p.hot_loops().collect();
        assert_eq!(hot, vec![(1, 5)]);
    }

    #[test]
    fn threshold_is_inclusive() {
        let mut p = LoopProfile::new(2, 3);
        p.record_loop_iter(0);
        p.record_loop_iter(0);
        p.record_loop_iter(0);
        let hot: Vec<_> = p.hot_loops().collect();
        assert_eq!(hot, vec![(0, 3)]);
    }
}
