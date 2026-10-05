//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Raw writes cannot invalidate a population selected before or after the writer's original snapshot.

use super::{
    invalid, OwnedPopulationMutation, PopulationLifecycle, PreparedRecordCommit, Reconciliation,
    VersionResult,
};

impl Reconciliation<'_> {
    pub(super) fn validate_invalidations(
        &self,
        origins: &PreparedRecordCommit,
        lifecycle: &PopulationLifecycle<'_>,
    ) -> VersionResult<()> {
        let mut writes = origins.writes();
        while let Some(write) = writes.next_metadata(self.control)? {
            if write.kind() != crate::mvcc::commit::RecordWriteKind::DiskANNOrigin || write.live() {
                continue;
            }
            self.control.check()?;
            let prefix = self
                .layout
                .origin_header_prefix(write.key(), self.control)?;
            let reject = || invalid("raw vector mutation would invalidate a DiskANN population");
            self.after.visit_keys(
                &prefix,
                None,
                usize::MAX,
                self.control,
                &mut |key, record| {
                    self.control.check()?;
                    if self.population_requires_origin(key, record.live, lifecycle)? {
                        return Err(reject());
                    }
                    Ok(true)
                },
            )?;
        }
        Ok(())
    }

    fn population_requires_origin(
        &self,
        key: &[u8],
        live: bool,
        lifecycle: &PopulationLifecycle<'_>,
    ) -> VersionResult<bool> {
        if let Some(write) = self.structural_header(key)? {
            return Ok(write.value().is_some());
        }
        Ok(match lifecycle.get(key) {
            // A new publication validates the entire final canonical view in its census. A structural copy above does not perform that validation.
            Some(
                OwnedPopulationMutation::Publish { .. } | OwnedPopulationMutation::Retire { .. },
            ) => false,
            None => live,
        })
    }
}
