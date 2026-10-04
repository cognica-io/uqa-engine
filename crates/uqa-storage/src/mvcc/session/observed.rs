//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Identifier watermarks a session has read back from its own observations.
//!
//! A watermark never lowers and an observation only raises it to the observed value, so an observation at or below a watermark this session has already read changes nothing and needs no physical allocation. A row rewrite observes the identity its row already has, and without this every rewritten row would pay one physical commit to change nothing.

/// Namespaces remembered at once. A session writes few tables at a time; the least recently observed namespace makes room for a new one.
const NAMESPACES: usize = 64;
/// Namespaces longer than this are not remembered, which bounds what the session retains.
const NAMESPACE_BYTES: usize = 256;

#[derive(Default)]
pub(super) struct ObservedWatermarks {
    /// Least recently observed first.
    watermarks: Vec<(Box<[u8]>, u64)>,
}

impl ObservedWatermarks {
    /// Whether the watermark of `namespace` is known to be at least `value`.
    pub(super) fn covers(&mut self, namespace: &[u8], value: u64) -> bool {
        let Some(position) = self
            .watermarks
            .iter()
            .position(|(known, _)| **known == *namespace)
        else {
            return false;
        };
        let entry = self.watermarks.remove(position);
        let covers = entry.1 >= value;
        self.watermarks.push(entry);
        covers
    }

    /// Remember that the watermark of `namespace` was `watermark` after a physical allocation.
    pub(super) fn record(&mut self, namespace: &[u8], watermark: u64) {
        if namespace.len() > NAMESPACE_BYTES {
            return;
        }
        if let Some(position) = self
            .watermarks
            .iter()
            .position(|(known, _)| **known == *namespace)
        {
            let mut entry = self.watermarks.remove(position);
            entry.1 = entry.1.max(watermark);
            self.watermarks.push(entry);
            return;
        }
        if self.watermarks.len() == NAMESPACES {
            self.watermarks.remove(0);
        }
        self.watermarks.push((namespace.into(), watermark));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recorded_watermark_covers_only_values_at_or_below_it() {
        let mut observed = ObservedWatermarks::default();
        assert!(!observed.covers(b"documents", 0));
        observed.record(b"documents", 10);
        assert!(observed.covers(b"documents", 10));
        assert!(observed.covers(b"documents", 0));
        assert!(!observed.covers(b"documents", 11));
        assert!(!observed.covers(b"vertices", 1));
        // A later allocation that reports a lower watermark cannot lower what is known.
        observed.record(b"documents", 4);
        assert!(observed.covers(b"documents", 10));
        observed.record(b"documents", 12);
        assert!(observed.covers(b"documents", 12));
    }

    #[test]
    fn the_least_recently_observed_namespace_makes_room() {
        let mut observed = ObservedWatermarks::default();
        for namespace in 0..NAMESPACES as u64 {
            observed.record(&namespace.to_be_bytes(), namespace);
        }
        // Reading the first namespace makes the second the least recently observed.
        assert!(observed.covers(&0_u64.to_be_bytes(), 0));
        observed.record(b"another", 1);
        assert!(observed.covers(&0_u64.to_be_bytes(), 0));
        assert!(!observed.covers(&1_u64.to_be_bytes(), 0));
        assert!(observed.covers(b"another", 1));
        assert_eq!(observed.watermarks.len(), NAMESPACES);

        observed.record(&[7; NAMESPACE_BYTES + 1], 3);
        assert!(!observed.covers(&[7; NAMESPACE_BYTES + 1], 3));
    }
}
