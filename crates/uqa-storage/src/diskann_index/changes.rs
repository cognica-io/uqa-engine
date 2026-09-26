//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded journal reclamation compares immutable origins, never writer allocation order.

use super::format::{DiskANNCanonicalOrigin, DiskANNChangeIdentity, DiskANNGeneration};
use super::pages::DiskANNOriginReader;
use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};
use uqa_core::DocId;

/// Continuation within one selected generation. Advance it only after the enclosing mutation commits; restart a new pass after reaching the end to discover later insertions before this key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskANNPruneCursor {
    generation: DiskANNGeneration,
    after: DiskANNChangeIdentity,
}

/// One bounded page. At most 64 journal entries are examined even if a larger maximum is supplied; zero is invalid.
#[derive(Debug, Clone, Copy)]
pub struct DiskANNPruneRequest {
    pub after: Option<DiskANNPruneCursor>,
    pub max_records: usize,
}

/// Evaluated deletions, not a commit receipt. `next == None` means the end of this pass was observed, not that future writes cannot add earlier keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskANNPruneResult {
    pub examined: usize,
    pub removed: usize,
    pub next: Option<DiskANNPruneCursor>,
}

/// Provider adapter over one fixed committed canonical/journal view and its evaluated mutation batch. The provider guards the real catalog and selected head in that same batch before pruning. Keys are immutable mutation identities; callbacks must not advance the view.
pub trait DiskANNChangeJournal {
    fn next_after(
        &self,
        after: Option<DiskANNChangeIdentity>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNChangeIdentity>>;
    fn change(
        &self,
        identity: DiskANNChangeIdentity,
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNCanonicalOrigin>;
    fn current_origin(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNCanonicalOrigin>>;
    fn remove(
        &mut self,
        identity: DiskANNChangeIdentity,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()>;
}

pub(crate) fn prune(
    origins: &DiskANNOriginReader,
    journal: &mut dyn DiskANNChangeJournal,
    request: DiskANNPruneRequest,
    control: &StorageReadControl,
) -> StorageBackendResult<DiskANNPruneResult> {
    control.check()?;
    let generation = origins.manifest().input().generation;
    if request.max_records == 0
        || request
            .after
            .is_some_and(|cursor| cursor.generation != generation)
    {
        return Err(invalid("invalid pruning limit or generation cursor"));
    }
    let mut after = request.after.map(|cursor| cursor.after);
    let mut result = DiskANNPruneResult {
        examined: 0,
        removed: 0,
        next: None,
    };
    for _ in 0..request.max_records.min(64) {
        control.check()?;
        let Some(identity) = journal.next_after(after, control)? else {
            result.next = None;
            control.check()?;
            return Ok(result);
        };
        if after.is_some_and(|last| last.encode() >= identity.encode()) {
            return Err(invalid("journal pruning cursor did not advance"));
        }
        let change = journal.change(identity, control)?;
        if change.version() != identity.version() {
            return Err(invalid(
                "journal payload differs from its mutation identity",
            ));
        }
        let current = journal.current_origin(identity.document(), control)?;
        if current.is_some_and(|origin| origin.version() == identity.version() && origin != change)
        {
            return Err(invalid("journal payload differs from the current origin"));
        }
        let covered = origins.origin(identity.document(), control)?;
        if covered.is_some_and(|origin| origin.version() == identity.version() && origin != change)
        {
            return Err(invalid("journal payload differs from the published origin"));
        }
        // A committed mutation never regains a superseded origin: undo cannot reuse revisions and receipt recovery never reevaluates writes. Older readers retain historical journal rows through MVCC.
        if covered == Some(change)
            || current.map(DiskANNCanonicalOrigin::version) != Some(identity.version())
        {
            journal.remove(identity, control)?;
            result.removed += 1;
        }
        result.examined += 1;
        after = Some(identity);
        result.next = Some(DiskANNPruneCursor {
            generation,
            after: identity,
        });
    }
    control.check()?;
    Ok(result)
}

fn invalid(message: &'static str) -> StorageBackendError {
    crate::mvcc::VersionError::InvalidEncoding(message).into_storage_error()
}
