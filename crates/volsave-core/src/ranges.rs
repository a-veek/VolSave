use std::ops::Range;

/// Sorted, non-overlapping byte ranges, e.g. the used parts of a volume.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RangeSet {
    /// Invariant: sorted by start, non-empty, neither overlapping nor adjacent.
    ranges: Vec<Range<u64>>,
}

impl RangeSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// A set covering `0..len`.
    pub fn full(len: u64) -> Self {
        Self::from_ranges(std::iter::once(0..len))
    }

    pub fn from_ranges(ranges: impl IntoIterator<Item = Range<u64>>) -> Self {
        let mut sorted: Vec<_> = ranges.into_iter().filter(|r| r.start < r.end).collect();
        sorted.sort_unstable_by_key(|r| r.start);
        let mut set = Self::new();
        for range in sorted {
            set.push(range);
        }
        set
    }

    /// Appends a range that starts at or after the start of the last one.
    /// Cheaper than [`Self::from_ranges`] when ranges arrive in order.
    ///
    /// # Panics
    /// If `range` starts before the last range.
    pub fn push(&mut self, range: Range<u64>) {
        if range.start >= range.end {
            return;
        }
        match self.ranges.last_mut() {
            Some(last) if range.start <= last.end => {
                assert!(range.start >= last.start, "ranges must be pushed in order");
                last.end = last.end.max(range.end);
            }
            _ => self.ranges.push(range),
        }
    }

    pub fn ranges(&self) -> &[Range<u64>] {
        &self.ranges
    }

    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    pub fn total_len(&self) -> u64 {
        self.ranges.iter().map(|r| r.end - r.start).sum()
    }

    pub fn union(&self, other: &RangeSet) -> RangeSet {
        Self::from_ranges(self.ranges.iter().chain(&other.ranges).cloned())
    }

    /// The parts of `self` not covered by `other`.
    pub fn subtract(&self, other: &RangeSet) -> RangeSet {
        let mut result = RangeSet::new();
        let mut cut = other.ranges.iter().peekable();
        for range in &self.ranges {
            let mut start = range.start;
            while let Some(c) = cut.peek() {
                if c.end <= start {
                    cut.next();
                    continue;
                }
                if c.start >= range.end {
                    break;
                }
                result.push(start..c.start.max(start));
                start = c.end;
                if c.end > range.end {
                    break;
                }
                cut.next();
            }
            result.push(start..range.end);
        }
        result
    }

    /// The parts of `self` inside `bound`.
    pub fn clamp(&self, bound: Range<u64>) -> RangeSet {
        Self::from_ranges(
            self.ranges
                .iter()
                .map(|r| r.start.max(bound.start)..r.end.min(bound.end)),
        )
    }

    /// Splits the set into image blocks of `block_size`, aligned to multiples
    /// of `block_size`. Blocks without any used byte are skipped.
    pub fn plan(&self, block_size: u32) -> BlockPlan<'_> {
        assert!(block_size > 0, "block size must not be 0");
        BlockPlan {
            ranges: &self.ranges,
            next_range: 0,
            cursor: 0,
            block_size: u64::from(block_size),
        }
    }
}

/// One image block: `start..start + len`, of which only `parts` hold data
/// worth reading; the rest is stored as zeros.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedBlock {
    pub start: u64,
    pub len: u32,
    pub parts: Vec<Range<u64>>,
}

pub struct BlockPlan<'a> {
    ranges: &'a [Range<u64>],
    next_range: usize,
    cursor: u64,
    block_size: u64,
}

impl Iterator for BlockPlan<'_> {
    type Item = PlannedBlock;

    fn next(&mut self) -> Option<PlannedBlock> {
        while self.ranges.get(self.next_range)?.end <= self.cursor {
            self.next_range += 1;
        }
        let first = &self.ranges[self.next_range];
        let pos = first.start.max(self.cursor);
        let start = pos - pos % self.block_size;
        // The block ends at the next boundary or where the data ends.
        let last_end = self.ranges.last()?.end;
        let end = (start + self.block_size).min(last_end);

        let mut parts = Vec::new();
        for range in &self.ranges[self.next_range..] {
            if range.start >= end {
                break;
            }
            parts.push(range.start.max(start)..range.end.min(end));
        }
        self.cursor = end;
        Some(PlannedBlock {
            start,
            len: (end - start) as u32,
            parts,
        })
    }
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)] // one-range sets are intended here
mod tests {
    use super::*;

    #[test]
    fn from_ranges_merges_and_sorts() {
        let set = RangeSet::from_ranges([10..20, 0..5, 5..7, 15..30, 40..40]);
        assert_eq!(set.ranges(), &[0..7, 10..30]);
        assert_eq!(set.total_len(), 27);
    }

    #[test]
    fn subtract_cuts_holes() {
        let set = RangeSet::from_ranges([0..100, 200..300]);
        let cut = RangeSet::from_ranges([10..20, 90..210, 250..260, 400..500]);
        assert_eq!(
            set.subtract(&cut).ranges(),
            &[0..10, 20..90, 210..250, 260..300]
        );
        assert_eq!(set.subtract(&RangeSet::full(1000)), RangeSet::new());
        assert_eq!(set.subtract(&RangeSet::new()), set);
    }

    #[test]
    fn union_and_clamp() {
        let a = RangeSet::from_ranges([0..10]);
        let b = RangeSet::from_ranges([10..20, 30..40]);
        assert_eq!(a.union(&b).ranges(), &[0..20, 30..40]);
        assert_eq!(a.union(&b).clamp(5..35).ranges(), &[5..20, 30..35]);
    }

    #[test]
    fn plan_skips_unused_blocks_and_clips_parts() {
        let set = RangeSet::from_ranges([0..10, 100..150, 350..360]);
        let plan: Vec<_> = set.plan(100).collect();
        assert_eq!(
            plan,
            vec![
                PlannedBlock {
                    start: 0,
                    len: 100,
                    parts: vec![0..10]
                },
                PlannedBlock {
                    start: 100,
                    len: 100,
                    parts: vec![100..150]
                },
                PlannedBlock {
                    start: 300,
                    len: 60,
                    parts: vec![350..360]
                },
            ]
        );
    }

    #[test]
    fn plan_splits_long_ranges() {
        let set = RangeSet::from_ranges([50..260]);
        let plan: Vec<_> = set.plan(100).collect();
        assert_eq!(plan.len(), 3);
        assert_eq!(plan[0].parts, vec![50..100]);
        assert_eq!(plan[1].parts, vec![100..200]);
        assert_eq!(
            plan[2],
            PlannedBlock {
                start: 200,
                len: 60,
                parts: vec![200..260]
            }
        );
    }
}
