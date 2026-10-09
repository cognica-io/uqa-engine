//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Worker-local decoded metadata follows durable revisions and relation lifetimes.

use std::collections::{BTreeMap, BTreeSet};
use uqa_storage::StorageBackendResult;

use super::MaintenanceState;

struct Entry {
    object_id: [u8; 16],
    revision: u64,
    state: MaintenanceState,
}

#[derive(Default)]
pub(super) struct MaintenanceCache {
    entries: BTreeMap<String, Entry>,
    completed: Option<CompletedPass>,
    #[cfg(test)]
    pub(super) full_passes: usize,
}

struct CompletedPass {
    version: u64,
    next_due_at: Option<u64>,
}

impl MaintenanceCache {
    pub(super) fn unchanged(&self, version: Option<u64>, now: u64) -> bool {
        self.completed.as_ref().is_some_and(|pass| {
            version == Some(pass.version) && pass.next_due_at.is_none_or(|deadline| now < deadline)
        })
    }

    pub(super) fn begin_pass(&mut self) {
        // A failed, interrupted or concurrently changed pass never suppresses a retry.
        self.completed = None;
        #[cfg(test)]
        {
            self.full_passes += 1;
        }
    }

    pub(super) fn complete_pass(&mut self, version: u64, next_due_at: Option<u64>) {
        self.completed = Some(CompletedPass {
            version,
            next_due_at,
        });
    }

    pub(super) fn retain<'a>(&mut self, names: impl Iterator<Item = &'a str>) {
        let names = names.collect::<BTreeSet<_>>();
        self.entries.retain(|name, _| names.contains(name.as_str()));
    }

    pub(super) fn remove(&mut self, name: &str) {
        self.entries.remove(name);
    }

    pub(super) fn load(
        &mut self,
        name: &str,
        object_id: [u8; 16],
        revision: Option<u64>,
        load: impl FnOnce() -> StorageBackendResult<(MaintenanceState, bool)>,
    ) -> StorageBackendResult<MaintenanceState> {
        if let Some(entry) = self
            .entries
            .get(name)
            .filter(|entry| entry.object_id == object_id && revision == Some(entry.revision))
        {
            return Ok(entry.state.clone());
        }
        // An error or an untracked provider must not leave an older entry available for a later lookup.
        self.entries.remove(name);
        let (state, stable) = load()?;
        if let Some(revision) = revision.filter(|_| stable) {
            self.entries.insert(
                name.to_owned(),
                Entry {
                    object_id,
                    revision,
                    state: state.clone(),
                },
            );
        }
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_completed_pass_requires_a_known_unchanged_version_and_an_unexpired_deadline() {
        let mut cache = MaintenanceCache::default();
        assert!(!cache.unchanged(Some(1), 0));
        cache.complete_pass(1, None);
        assert!(cache.unchanged(Some(1), u64::MAX));
        assert!(!cache.unchanged(None, 0));
        assert!(!cache.unchanged(Some(2), 0));
        cache.complete_pass(1, Some(60_100));
        assert!(cache.unchanged(Some(1), 60_099));
        assert!(!cache.unchanged(Some(1), 60_100));
        cache.begin_pass();
        assert!(!cache.unchanged(Some(1), 60_099));
    }

    #[test]
    fn unchanged_polls_decode_once_and_revisions_objects_and_removal_invalidate() {
        let mut cache = MaintenanceCache::default();
        let mut loads = 0;
        for (object, revision) in [([1; 16], 1), ([1; 16], 1), ([1; 16], 2), ([2; 16], 2)] {
            cache
                .load("items", object, Some(revision), || {
                    loads += 1;
                    Ok((MaintenanceState::default(), true))
                })
                .unwrap();
        }
        assert_eq!(loads, 3);
        cache.retain(std::iter::empty());
        cache
            .load("items", [2; 16], Some(2), || {
                loads += 1;
                Ok((MaintenanceState::default(), true))
            })
            .unwrap();
        assert_eq!(loads, 4);
        cache.remove("items");
        assert!(cache.entries.is_empty());
    }

    #[test]
    fn untracked_unstable_and_failed_reads_never_populate_the_cache() {
        let mut cache = MaintenanceCache::default();
        for (revision, stable) in [(None, true), (Some(1), false)] {
            cache
                .load("items", [1; 16], revision, || {
                    Ok((MaintenanceState::default(), stable))
                })
                .unwrap();
            assert!(cache.entries.is_empty());
        }
        cache
            .load("items", [1; 16], Some(1), || {
                Ok((MaintenanceState::default(), true))
            })
            .unwrap();
        assert!(cache
            .load("items", [1; 16], Some(2), || Err(
                uqa_storage::StorageBackendError::Other("test failure".into())
            ))
            .is_err());
        assert!(cache.entries.is_empty());
    }

    #[test]
    fn cached_metadata_keeps_time_based_due_checks_live() {
        let mut cache = MaintenanceCache::default();
        let state: MaintenanceState = serde_json::from_str(
            r#"{"changes":1,"dirty_since_ms":10,"analyzed_rows":100,"statistics_format":1}"#,
        )
        .unwrap();
        cache
            .load("items", [1; 16], Some(1), || Ok((state, true)))
            .unwrap();
        let cached = cache
            .load("items", [1; 16], Some(1), || {
                panic!("unchanged metadata must be reused")
            })
            .unwrap();
        assert!(!cached.due(false, 60_009, 1));
        assert!(cached.due(false, 60_010, 1));
    }
}
