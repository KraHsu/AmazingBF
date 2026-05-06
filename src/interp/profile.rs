//! Runtime hotspot profiling for the interpreter.
//!
//! Loop profiling records one trip every time a `LoopEnd` back-edge fires
//! (i.e. `*p != 0` at `]`). Opcode profiling records dispatch counts by
//! [`crate::interp::bytecode::InterpOp`] tag. Both are optional: normal
//! interpreter runs leave the profile unset and pay no counter-update cost.

use crate::interp::bytecode::{INTERP_OP_TAG_COUNT, INTERP_OP_TAG_NAMES};

/// Number of entries shown in the debug hotspot report.
pub(crate) const DEFAULT_HOTSPOT_TOP_N: usize = 8;

/// Per-loop trip count, keyed by the `LoopStart` pc index.
#[derive(Debug, Clone)]
pub(crate) struct LoopProfile {
    /// Trip counts indexed by `LoopStart` pc. Only loops that have been
    /// entered at least once have a non-zero entry.
    counts: Vec<u64>,
    /// Threshold above which a loop is considered "hot".
    threshold: u64,
    /// Dispatch counts indexed by `InterpOp::tag()`.
    opcode_counts: [u64; INTERP_OP_TAG_COUNT],
    /// Whether opcode counts should be recorded by the dispatch loop.
    opcode_enabled: bool,
}

/// One loop row in a hotspot report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LoopHotspot {
    pub(crate) start_pc: u32,
    pub(crate) trip_count: u64,
}

/// One opcode row in a hotspot report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OpcodeHotspot {
    pub(crate) tag: usize,
    pub(crate) name: &'static str,
    pub(crate) count: u64,
}

impl LoopProfile {
    /// Create a new profile with `num_ops` slots (one per bytecode op).
    pub(crate) fn new(num_ops: usize, threshold: u64) -> Self {
        Self {
            counts: vec![0; num_ops],
            threshold,
            opcode_counts: [0; INTERP_OP_TAG_COUNT],
            opcode_enabled: false,
        }
    }

    /// Enable opcode dispatch counters for diagnostic hotspot reports.
    pub(crate) fn enable_opcode_counts(&mut self) {
        self.opcode_enabled = true;
    }

    /// Returns whether opcode dispatch counters are enabled.
    #[inline]
    pub(crate) fn opcode_counts_enabled(&self) -> bool {
        self.opcode_enabled
    }

    /// Record one dispatched interpreter opcode.
    #[inline]
    pub(crate) fn record_opcode(&mut self, tag: usize) {
        self.opcode_counts[tag] += 1;
    }

    /// Record one back-edge execution for the loop whose `LoopStart` is at
    /// `start_pc`.
    #[inline]
    pub(crate) fn record_back_edge(&mut self, start_pc: u32) {
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

    /// Return the hottest loops by trip count, descending.
    pub(crate) fn top_loops(&self, limit: usize) -> Vec<LoopHotspot> {
        let mut rows: Vec<_> = self
            .counts
            .iter()
            .enumerate()
            .filter(|&(_, &count)| count > 0)
            .map(|(pc, &count)| LoopHotspot {
                start_pc: pc as u32,
                trip_count: count,
            })
            .collect();
        rows.sort_by(|a, b| {
            b.trip_count
                .cmp(&a.trip_count)
                .then_with(|| a.start_pc.cmp(&b.start_pc))
        });
        rows.truncate(limit);
        rows
    }

    /// Return the hottest opcodes by dispatch count, descending.
    pub(crate) fn top_opcodes(&self, limit: usize) -> Vec<OpcodeHotspot> {
        let mut rows: Vec<_> = self
            .opcode_counts
            .iter()
            .enumerate()
            .filter(|&(_, &count)| count > 0)
            .map(|(tag, &count)| OpcodeHotspot {
                tag,
                name: INTERP_OP_TAG_NAMES[tag],
                count,
            })
            .collect();
        rows.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.tag.cmp(&b.tag)));
        rows.truncate(limit);
        rows
    }

    /// Total loop back-edges observed.
    pub(crate) fn total_loop_trips(&self) -> u64 {
        self.counts.iter().sum()
    }

    /// Total opcode dispatches observed.
    pub(crate) fn total_opcode_dispatches(&self) -> u64 {
        self.opcode_counts.iter().sum()
    }

    /// The configured hot threshold.
    pub(crate) fn threshold(&self) -> u64 {
        self.threshold
    }
}

/// Format the user-facing interpreter hotspot report printed by
/// `--interp-debug`.
pub(crate) fn format_hotspot_report(profile: &LoopProfile, top_n: usize) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "[interp-debug] hotspots total_opcode_dispatches={} total_loop_trips={} hot_threshold={}\n",
        profile.total_opcode_dispatches(),
        profile.total_loop_trips(),
        profile.threshold(),
    ));

    out.push_str("[interp-debug] top_opcodes");
    let total_ops = profile.total_opcode_dispatches();
    let op_rows = profile.top_opcodes(top_n);
    if op_rows.is_empty() {
        out.push_str(" none\n");
    } else {
        out.push('\n');
        for (rank, row) in op_rows.iter().enumerate() {
            out.push_str(&format!(
                "[interp-debug]   #{:02} opcode={} tag={} count={} share={:.2}%\n",
                rank + 1,
                row.name,
                row.tag,
                row.count,
                pct(row.count, total_ops),
            ));
        }
    }

    out.push_str("[interp-debug] top_loops");
    let total_loops = profile.total_loop_trips();
    let loop_rows = profile.top_loops(top_n);
    if loop_rows.is_empty() {
        out.push_str(" none\n");
    } else {
        out.push('\n');
        for (rank, row) in loop_rows.iter().enumerate() {
            out.push_str(&format!(
                "[interp-debug]   #{:02} loop_start_pc={} trips={} share={:.2}%\n",
                rank + 1,
                row.start_pc,
                row.trip_count,
                pct(row.trip_count, total_loops),
            ));
        }
    }

    out
}

fn pct(part: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (part as f64) * 100.0 / (total as f64)
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
        assert_eq!(p.total_opcode_dispatches(), 0);
        assert_eq!(p.total_loop_trips(), 0);
        assert!(!p.opcode_counts_enabled());
    }

    #[test]
    fn record_increments_correct_slot() {
        let mut p = LoopProfile::new(5, 10);
        p.record_back_edge(2);
        p.record_back_edge(2);
        p.record_back_edge(4);
        assert_eq!(p.trip_count(0), 0);
        assert_eq!(p.trip_count(2), 2);
        assert_eq!(p.trip_count(4), 1);
        assert_eq!(p.total_loop_trips(), 3);
    }

    #[test]
    fn hot_loops_filters_by_threshold() {
        let mut p = LoopProfile::new(4, 3);
        for _ in 0..5 {
            p.record_back_edge(1);
        }
        for _ in 0..2 {
            p.record_back_edge(3);
        }
        let hot: Vec<_> = p.hot_loops().collect();
        assert_eq!(hot, vec![(1, 5)]);
    }

    #[test]
    fn threshold_is_inclusive() {
        let mut p = LoopProfile::new(2, 3);
        p.record_back_edge(0);
        p.record_back_edge(0);
        p.record_back_edge(0);
        let hot: Vec<_> = p.hot_loops().collect();
        assert_eq!(hot, vec![(0, 3)]);
    }

    #[test]
    fn top_loops_sort_by_count_then_pc() {
        let mut p = LoopProfile::new(6, 1);
        p.record_back_edge(4);
        p.record_back_edge(2);
        p.record_back_edge(4);
        p.record_back_edge(1);
        p.record_back_edge(1);

        assert_eq!(
            p.top_loops(2),
            vec![
                LoopHotspot {
                    start_pc: 1,
                    trip_count: 2,
                },
                LoopHotspot {
                    start_pc: 4,
                    trip_count: 2,
                },
            ]
        );
    }

    #[test]
    fn opcode_counts_are_opt_in_and_ranked() {
        let mut p = LoopProfile::new(1, 1);
        p.enable_opcode_counts();
        p.record_opcode(1);
        p.record_opcode(1);
        p.record_opcode(0);

        assert!(p.opcode_counts_enabled());
        assert_eq!(p.total_opcode_dispatches(), 3);
        assert_eq!(
            p.top_opcodes(2),
            vec![
                OpcodeHotspot {
                    tag: 1,
                    name: "Add",
                    count: 2,
                },
                OpcodeHotspot {
                    tag: 0,
                    name: "Move",
                    count: 1,
                },
            ]
        );
    }

    #[test]
    fn report_includes_summary_and_ranked_rows() {
        let mut p = LoopProfile::new(3, 7);
        p.enable_opcode_counts();
        p.record_opcode(1);
        p.record_opcode(1);
        p.record_back_edge(2);

        let report = format_hotspot_report(&p, 4);
        assert!(report.contains("hotspots total_opcode_dispatches=2 total_loop_trips=1"));
        assert!(report.contains("opcode=Add tag=1 count=2 share=100.00%"));
        assert!(report.contains("loop_start_pc=2 trips=1 share=100.00%"));
    }
}
