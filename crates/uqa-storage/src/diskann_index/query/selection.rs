//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sparse document selections compose actual retained canonical sources without replacing the physical reader.

mod source;
pub use source::DiskANNReadSnapshot;

use super::invalid;
use crate::diskann_index::{
    format::{DiskANNCanonicalOrigin, DiskANNChangeIdentity, DiskANNVectorVersion},
    DiskANNCanonicalRead, DiskANNCanonicalVectorVisitor, DiskANNQueryRead,
};
use crate::{read_control::StorageReadControl, StorageBackendResult};
use std::sync::Arc;
use uqa_core::{
    memory::{Budgeted, BudgetedVec},
    DocId,
};

type SelectedSources = Arc<Budgeted<BudgetedVec<(DocId, Option<DiskANNReadSnapshot>)>>>;

/// Immutable, ordered replacements selected from actual `DiskANN` snapshots. None selects an evaluated deletion independently of retained source membership. A selected document absent from its source is also deleted; a zero-ordinal origin remains an explicit empty replacement. None of these cases reads the original document. Clones share the sparse directory and its original allowance.
#[derive(Clone)]
pub struct DiskANNReadChanges {
    sources: SelectedSources,
    control: StorageReadControl,
}

impl DiskANNReadChanges {
    /// Capture a strictly increasing document selection. Callers supply the already evaluated private source for each replacement and None for each deletion; unselected documents remain on the original view.
    pub fn capture(
        sources: impl IntoIterator<Item = StorageBackendResult<(DocId, Option<DiskANNReadSnapshot>)>>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let mut selected = BudgetedVec::new(control.memory());
        let mut after = None;
        for entry in sources {
            control.check()?;
            let (document, source) = entry?;
            if after.is_some_and(|after| document <= after) {
                return Err(invalid(
                    "private document selection is not strictly increasing",
                ));
            }
            if let Some(source) = &source {
                source.check_control(control)?;
            }
            selected.push((document, source))?;
            after = Some(document);
        }
        let sources =
            Budgeted::new(selected, control.memory().empty_reservation()).into_shared()?;
        control.check()?;
        Ok(Self {
            sources,
            control: control.clone(),
        })
    }

    fn source(&self, document: DocId) -> Option<&Option<DiskANNReadSnapshot>> {
        self.sources
            .binary_search_by_key(&document, |(id, _)| *id)
            .ok()
            .map(|position| &self.sources[position].1)
    }

    fn next(&self, after: Option<DocId>) -> Option<DocId> {
        let position = self
            .sources
            .partition_point(|(document, _)| after.is_some_and(|after| *document <= after));
        self.sources.get(position).map(|(document, _)| *document)
    }
}

pub(super) struct SelectedCanonical {
    base: DiskANNReadSnapshot,
    changes: DiskANNReadChanges,
}

impl SelectedCanonical {
    pub(super) fn new(
        base: DiskANNReadSnapshot,
        changes: &DiskANNReadChanges,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        base.check_control(control)?;
        changes.control.check()?;
        for source in changes
            .sources
            .iter()
            .filter_map(|(_, source)| source.as_ref())
        {
            source.check_control(control)?;
            if !base.same_lineage(source) || base.dimensions() != source.dimensions() {
                return Err(invalid(
                    "private canonical source belongs to a different index lineage or width",
                ));
            }
        }
        Ok(Self {
            base,
            changes: changes.clone(),
        })
    }

    fn source(&self, document: DocId) -> Option<&DiskANNReadSnapshot> {
        self.changes
            .source(document)
            .map_or(Some(&self.base), Option::as_ref)
    }
}

impl DiskANNCanonicalRead for SelectedCanonical {
    fn check_control(&self, control: &StorageReadControl) -> StorageBackendResult<()> {
        self.changes.control.check()?;
        self.base.check_control(control)
    }

    fn dimensions(&self) -> u32 {
        self.base.dimensions()
    }

    fn next_document_after(
        &self,
        mut after: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DocId>> {
        loop {
            self.check_control(control)?;
            let base = self.base.next_document_after(after, control)?;
            let selected = self.changes.next(after);
            let Some(document) = first(base, selected) else {
                return Ok(None);
            };
            if after.is_some_and(|after| document <= after) {
                return Err(invalid("private canonical document cursor did not advance"));
            }
            // Preserve the base cursor's integrity checks, including unstamped values. Only a selected, genuinely absent replacement removes an enumerated identity.
            if self.changes.source(document).is_none() || self.origin(document, control)?.is_some()
            {
                return Ok(Some(document));
            }
            after = Some(document);
        }
    }

    fn origin(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        self.check_control(control)?;
        self.source(document)
            .map(|source| source.origin(document, control))
            .transpose()
            .map(Option::flatten)
    }

    fn visit_document(
        &self,
        document: DocId,
        control: &StorageReadControl,
        visit: &mut DiskANNCanonicalVectorVisitor<'_>,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        self.check_control(control)?;
        self.source(document)
            .map(|source| source.visit_document(document, control, visit))
            .transpose()
            .map(Option::flatten)
    }
}

impl DiskANNQueryRead for SelectedCanonical {
    fn document_origin(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNCanonicalOrigin>> {
        self.check_control(control)?;
        self.source(document)
            .map(|source| source.document_origin(document, control))
            .transpose()
            .map(Option::flatten)
    }

    fn next_change_after(
        &self,
        mut after: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNChangeIdentity>> {
        loop {
            self.check_control(control)?;
            let base = self.base.next_change_after(after, control)?;
            let selected = self.changes.next(after);
            let Some(document) = first(base.map(DiskANNChangeIdentity::document), selected) else {
                return Ok(None);
            };
            if after.is_some_and(|after| document <= after) {
                return Err(invalid("private canonical change cursor did not advance"));
            }
            if let Some(source) = self.changes.source(document) {
                // A private source may already have compacted its own journal. Its actual origin still marks coverage relative to the retained base generation.
                if let Some(version) = source
                    .as_ref()
                    .map(|source| source.origin(document, control))
                    .transpose()?
                    .flatten()
                {
                    return Ok(Some(DiskANNChangeIdentity::new(document, version)));
                }
            } else {
                return Ok(base);
            }
            after = Some(document);
        }
    }
}

fn first(left: Option<DocId>, right: Option<DocId>) -> Option<DocId> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (left, right) => left.or(right),
    }
}
