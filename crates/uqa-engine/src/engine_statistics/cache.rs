//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Share decoded committed statistics, with a separate load gate per relation.

use std::collections::BTreeMap;
use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use uqa_storage::StorageBackendResult;

use crate::ColumnStatsMap;

struct Snapshot {
    object_id: [u8; 16],
    revision: u64,
    statistics: Weak<ColumnStatsMap>,
}

type Slot = Arc<Mutex<Option<Snapshot>>>;

#[derive(Default)]
pub(crate) struct StatisticsSnapshots {
    slots: Mutex<BTreeMap<String, Slot>>,
}

impl StatisticsSnapshots {
    pub(crate) fn load(
        &self,
        name: &str,
        object_id: [u8; 16],
        revision: u64,
        load: impl FnOnce() -> StorageBackendResult<ColumnStatsMap>,
    ) -> StorageBackendResult<Arc<ColumnStatsMap>> {
        let slot = Arc::clone(self.slots.lock().entry(name.to_owned()).or_default());
        let mut slot = slot.lock();
        if let Some(snapshot) = slot
            .as_ref()
            .filter(|snapshot| snapshot.object_id == object_id && snapshot.revision == revision)
        {
            if let Some(statistics) = snapshot.statistics.upgrade() {
                return Ok(statistics);
            }
        }
        let statistics = Arc::new(load()?);
        *slot = Some(Snapshot {
            object_id,
            revision,
            statistics: Arc::downgrade(&statistics),
        });
        Ok(statistics)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Barrier;

    use super::*;

    #[test]
    fn concurrent_readers_decode_each_committed_snapshot_once() {
        let snapshots = StatisticsSnapshots::default();
        let barrier = Barrier::new(8);
        let calls = AtomicUsize::new(0);
        let results = std::thread::scope(|scope| {
            let workers = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        snapshots
                            .load("public.items", [1; 16], 5, || {
                                calls.fetch_add(1, Ordering::AcqRel);
                                Ok(ColumnStatsMap::new())
                            })
                            .unwrap()
                    })
                })
                .collect::<Vec<_>>();
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(calls.load(Ordering::Acquire), 1);
        assert!(results
            .iter()
            .all(|result| Arc::ptr_eq(&results[0], result)));
        for (object, revision) in [([1; 16], 6), ([2; 16], 6)] {
            let changed = snapshots
                .load("public.items", object, revision, || {
                    calls.fetch_add(1, Ordering::AcqRel);
                    Ok(ColumnStatsMap::new())
                })
                .unwrap();
            assert!(!Arc::ptr_eq(&results[0], &changed));
        }
        assert_eq!(calls.load(Ordering::Acquire), 3);
    }

    #[test]
    fn failed_statistics_load_is_retried_without_poisoning_the_slot() {
        let snapshots = StatisticsSnapshots::default();
        assert!(snapshots
            .load("public.items", [1; 16], 1, || {
                Err(uqa_storage::StorageBackendError::Other(
                    "test load failure".into(),
                ))
            })
            .is_err());
        assert!(snapshots
            .load("public.items", [1; 16], 1, || { Ok(ColumnStatsMap::new()) })
            .is_ok());
    }
}
