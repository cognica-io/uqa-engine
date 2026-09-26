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

/// Continuation within one selected generation. A page with deletions advances only after commit; a zero-deletion page may advance after confirmed rollback. Restart a new pass after reaching the end to discover later insertions before this key.
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

/// One finite journal-discovery snapshot bound to the original catalog, selected generation and backend session. Each page evaluates current committed origins in the caller's active transaction. Deletions require confirmed commit before cursor advancement; a zero-deletion page permits confirmed rollback. Later journal keys wait for the next pass, and already-deleted keys still count toward the page limit.
pub trait DiskANNJournalPruner: Send + Sync {
    fn prune(
        &self,
        request: DiskANNPruneRequest,
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNPruneResult>;
}

/// Actual provider journal keys and complete canonical origins. Pruning may retain older discovery with current deletion evidence; exact statistics use one fixed view for every read. Callbacks must not advance either view.
pub trait DiskANNChangeRead {
    fn next_after(
        &self,
        after: Option<DiskANNChangeIdentity>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNChangeIdentity>>;
    /// Read the value on the supplied evidence view. Pruning uses its current transaction, where an older discovery key may already be absent; statistics use the same retained view for discovery and evidence.
    fn change(
        &self,
        identity: DiskANNChangeIdentity,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNCanonicalOrigin>>;
    fn current_origin(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNCanonicalOrigin>>;
}

/// Exact-key deletion shares the reader's publishing transaction and its catalog/head guards.
pub trait DiskANNChangeJournal {
    fn read(&self) -> &dyn DiskANNChangeRead;
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
        let Some(identity) = journal.read().next_after(after, control)? else {
            result.next = None;
            control.check()?;
            return Ok(result);
        };
        if after.is_some_and(|last| last.encode() >= identity.encode()) {
            return Err(invalid("journal pruning cursor did not advance"));
        }
        result.examined += 1;
        after = Some(identity);
        result.next = Some(DiskANNPruneCursor {
            generation,
            after: identity,
        });
        // A committed mutation never regains a superseded origin: undo cannot reuse revisions and receipt recovery never reevaluates writes. Older readers retain historical journal rows through MVCC.
        if matches!(
            classify(origins, journal.read(), identity, control)?,
            Change::Reclaimable
        ) {
            journal.remove(identity, control)?;
            result.removed += 1;
        }
    }
    control.check()?;
    Ok(result)
}

pub(super) enum Change {
    Missing,
    Reclaimable,
    Outstanding(DiskANNCanonicalOrigin),
}

pub(super) fn classify<J: DiskANNChangeRead + ?Sized>(
    origins: &DiskANNOriginReader,
    journal: &J,
    identity: DiskANNChangeIdentity,
    control: &StorageReadControl,
) -> StorageBackendResult<Change> {
    let Some(change) = journal.change(identity, control)? else {
        return Ok(Change::Missing);
    };
    if change.version() != identity.version() {
        return Err(invalid(
            "journal payload differs from its mutation identity",
        ));
    }
    if change.dimensions() != origins.manifest().input().dimensions {
        return Err(invalid(
            "journal dimensions differ from the published generation",
        ));
    }
    let current = journal.current_origin(identity.document(), control)?;
    if current.is_some_and(|origin| origin.version() == identity.version() && origin != change) {
        return Err(invalid("journal payload differs from the current origin"));
    }
    let covered = origins.origin(identity.document(), control)?;
    if covered.is_some_and(|origin| origin.version() == identity.version() && origin != change) {
        return Err(invalid("journal payload differs from the published origin"));
    }
    Ok(
        if covered == Some(change)
            || current.map(DiskANNCanonicalOrigin::version) != Some(identity.version())
        {
            Change::Reclaimable
        } else {
            Change::Outstanding(change)
        },
    )
}

fn invalid(message: &'static str) -> StorageBackendError {
    crate::mvcc::VersionError::InvalidEncoding(message).into_storage_error()
}
