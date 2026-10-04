//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lazy layers above a selection of changes: the rows a transaction changed, read at a fixed snapshot, and above them the rows running commands staged, each command's above the commands' before it. A read takes immutable views of the layers, so it copies none of their rows.

use super::identities::{ChangedIdentitiesView, ChangedRowCounts, IdentityCursor};
use super::vectors::CapturedRows;
use super::{
    Arc, Change, DocId, DocumentChanges, DocumentStore, StorageBackendResult, StorageReadControl,
};
use crate::mutation::overlay::{StagedCursor, StagedRow, StagedRowsView};
use uqa_core::memory::{Budgeted, BudgetedVec};

pub(super) struct StagedLayers {
    /// The commands' rows, the oldest command first.
    views: BudgetedVec<StagedRowsView>,
    /// The allowance that reads decode spilled rows into.
    pub(super) control: StorageReadControl,
}

/// The rows a transaction changed in one relation, read from the transaction's view of it.
pub(super) struct IdentityLayer {
    view: ChangedIdentitiesView,
    rows: IdentityRows,
    control: StorageReadControl,
}

/// Where the present rows of an identity layer are read.
pub(super) enum IdentityRows {
    Retained(Arc<dyn DocumentStore>),
    /// The rows with the vector sources the relation held when the layer was taken.
    Captured(Arc<Budgeted<CapturedRows>>),
}

impl IdentityLayer {
    /// The change of a changed row that is present now or not.
    fn change(&self, present: bool) -> Change {
        match (&self.rows, present) {
            (IdentityRows::Retained(source), true) => Change::Retained(Arc::clone(source)),
            (IdentityRows::Retained(_), false) => Change::Deleted,
            (IdentityRows::Captured(rows), present) => Change::Captured(Arc::clone(rows), present),
        }
    }

    /// The source of the present rows.
    fn source(&self) -> &Arc<dyn DocumentStore> {
        match &self.rows {
            IdentityRows::Retained(source) => source,
            IdentityRows::Captured(rows) => &rows.documents,
        }
    }
}

/// How a lazy layer changes one identity.
#[derive(Clone)]
enum LazyChange {
    /// A command staged this change.
    Staged(Change),
    /// The transaction changed the row, which is present now or not.
    Changed(bool),
    Unchanged,
}

/// Reads the lazy layers' changes of the identities of one batch, in the batch's order. Ascending identities advance each layer once over the batch, holding one bounded page of each; other batches look each identity up.
pub(super) struct LayerReader<'a> {
    changes: &'a DocumentChanges,
    ascending: bool,
    /// Each command's staged rows, the oldest first.
    heads: Vec<StagedHead<StagedRow>>,
    identity: Option<IdentityHead>,
    /// The identity read last and its change, which a lookahead and the read that follows it share.
    last: Option<(DocId, LazyChange)>,
}

struct IdentityHead {
    cursor: IdentityCursor,
    next: Option<(DocId, bool)>,
    done: bool,
}

impl IdentityHead {
    /// Advance to `id` and take its entry when the cursor holds one.
    fn take(
        &mut self,
        id: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<bool>> {
        while !self.done && self.next.is_none_or(|(next, _)| next < id) {
            self.next = self.cursor.next(control)?;
            self.done = self.next.is_none();
        }
        match self.next {
            Some((next, present)) if next == id => {
                self.next = None;
                Ok(Some(present))
            }
            _ => Ok(None),
        }
    }
}

impl LayerReader<'_> {
    fn lazy(&mut self, id: DocId) -> StorageBackendResult<LazyChange> {
        if let Some((last, change)) = &self.last {
            if *last == id {
                return Ok(change.clone());
            }
        }
        let change = if self.ascending {
            let mut found = None;
            if let Some(staged) = &self.changes.staged {
                for head in self.heads.iter_mut().rev() {
                    while !head.done && head.next.as_ref().is_none_or(|(next, _)| *next < id) {
                        head.next = head.cursor.next(&staged.control)?;
                        head.done = head.next.is_none();
                    }
                    if head.next.as_ref().is_some_and(|(next, _)| *next == id) {
                        let (_, row) = head.next.take().expect("a matched staged row");
                        if found.is_none() {
                            found = Some(staged_change(row));
                        }
                    }
                }
            }
            let changed = match (&mut self.identity, &self.changes.identities) {
                (Some(head), Some(layer)) => head.take(id, &layer.control)?,
                _ => None,
            };
            match (found, changed) {
                (Some(change), _) => LazyChange::Staged(change),
                (None, Some(present)) => LazyChange::Changed(present),
                (None, None) => LazyChange::Unchanged,
            }
        } else if let Some(change) = self.changes.staged_change(id)? {
            LazyChange::Staged(change)
        } else {
            self.changes
                .identity_presence(id)?
                .map_or(LazyChange::Unchanged, LazyChange::Changed)
        };
        self.last = Some((id, change.clone()));
        Ok(change)
    }
}

/// A change read from a selection, or produced by a lazy layer.
pub(super) enum ChangeRef<'a> {
    Borrowed(&'a Change),
    Owned(Change),
}

impl std::ops::Deref for ChangeRef<'_> {
    type Target = Change;

    fn deref(&self) -> &Change {
        match self {
            Self::Borrowed(change) => change,
            Self::Owned(change) => change,
        }
    }
}

impl ChangeRef<'_> {
    pub(super) fn into_owned(self) -> Change {
        match self {
            Self::Borrowed(change) => change.clone(),
            Self::Owned(change) => change,
        }
    }
}

/// The change a staged row makes.
fn staged_change(row: StagedRow) -> Change {
    row.map_or(Change::Deleted, |row| {
        Change::Fields(row.fields, row.metadata)
    })
}

impl DocumentChanges {
    /// These changes with the rows that views of running commands staged above them, the newest command's view last.
    pub(crate) fn with_staged(
        mut self,
        views: impl IntoIterator<Item = StagedRowsView>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        let mut layers = BudgetedVec::new(control.memory());
        for view in self.staged_views() {
            layers.push(view.clone())?;
        }
        for view in views {
            control.check()?;
            layers.push(view)?;
        }
        if !layers.is_empty() {
            self.staged = Some(Arc::new(StagedLayers {
                views: layers,
                control: control.clone(),
            }));
        }
        Ok(self)
    }

    /// These changes with the rows a transaction changed above them, below any rows that commands staged.
    pub(super) fn with_identity_layer(
        mut self,
        view: ChangedIdentitiesView,
        rows: IdentityRows,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        if self.staged.is_some() || self.identities.is_some() {
            return Err(super::selection::staged_below());
        }
        self.identities = Some(Arc::new(IdentityLayer {
            view,
            rows,
            control: control.clone(),
        }));
        Ok(self)
    }

    pub(super) fn staged_views(&self) -> &[StagedRowsView] {
        self.staged.as_ref().map_or(&[], |staged| &staged.views)
    }

    pub(super) fn has_identities(&self) -> bool {
        self.identities.is_some()
    }

    /// The counted rows a transaction changed, when these changes are exactly those rows, which a read then counts without visiting them.
    pub fn identity_counts(&self) -> Option<ChangedRowCounts> {
        if !self.rows().is_empty() || self.staged.is_some() {
            return None;
        }
        self.identities.as_ref().map(|layer| layer.view.counts())
    }

    /// The change the newest command staged for `id`.
    fn staged_change(&self, id: DocId) -> StorageBackendResult<Option<Change>> {
        let Some(staged) = &self.staged else {
            return Ok(None);
        };
        for view in staged.views.iter().rev() {
            if let Some(row) = view.get(id, &staged.control)? {
                return Ok(Some(staged_change(row)));
            }
        }
        Ok(None)
    }

    /// Whether a command staged a change for `id`, without decoding a spilled row.
    pub(super) fn stages(&self, id: DocId) -> StorageBackendResult<bool> {
        let Some(staged) = &self.staged else {
            return Ok(false);
        };
        for view in staged.views.iter() {
            if view.contains(id, &staged.control)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Whether the transaction changed the row `id`, and whether it is present now.
    pub(super) fn identity_presence(&self, id: DocId) -> StorageBackendResult<Option<bool>> {
        match &self.identities {
            Some(layer) => layer.view.presence(id, &layer.control),
            None => Ok(None),
        }
    }

    /// Whether a lazy layer changes `id`.
    pub(super) fn layers_change(&self, id: DocId) -> StorageBackendResult<bool> {
        Ok(self.stages(id)? || self.identity_presence(id)?.is_some())
    }

    /// A reader of the lazy layers' changes of the identities of the batch `ids`, or `None` when there is no lazy layer.
    pub(super) fn layer_reader(&self, ids: &[DocId]) -> Option<LayerReader<'_>> {
        if self.staged.is_none() && self.identities.is_none() {
            return None;
        }
        let ascending = ids.len() > 1 && ids.windows(2).all(|pair| pair[0] < pair[1]);
        let start = ids.first().and_then(|first| first.checked_sub(1));
        Some(LayerReader {
            changes: self,
            ascending,
            heads: if ascending {
                self.staged_views()
                    .iter()
                    .map(|view| StagedHead {
                        cursor: view.rows(start),
                        next: None,
                        done: false,
                    })
                    .collect()
            } else {
                Vec::new()
            },
            identity: self
                .identities
                .as_ref()
                .filter(|_| ascending)
                .map(|layer| IdentityHead {
                    cursor: layer.view.cursor(start),
                    next: None,
                    done: false,
                }),
            last: None,
        })
    }

    /// The newest change of `id`, an identity of the batch that `reader` reads.
    pub(super) fn batch_change<'a>(
        &'a self,
        reader: &mut Option<LayerReader<'a>>,
        id: DocId,
    ) -> StorageBackendResult<Option<ChangeRef<'a>>> {
        if let Some(reader) = reader {
            match reader.lazy(id)? {
                LazyChange::Staged(change) => return Ok(Some(ChangeRef::Owned(change))),
                LazyChange::Changed(present) => {
                    let layer = self.identities.as_ref().expect("an identity layer");
                    return Ok(Some(ChangeRef::Owned(layer.change(present))));
                }
                LazyChange::Unchanged => {}
            }
        }
        Ok(self.selected(id).map(ChangeRef::Borrowed))
    }

    /// The end of the run of `ids` from `start` whose rows one retained source supplies, with that source; `reader` reads the batch's lazy layers.
    pub(super) fn batch_source_run<'a>(
        &'a self,
        ids: &[DocId],
        start: usize,
        reader: &mut Option<LayerReader<'a>>,
    ) -> StorageBackendResult<Option<(usize, &'a Arc<dyn DocumentStore>)>> {
        let mut retained = |id: DocId| -> StorageBackendResult<Option<&'a Arc<dyn DocumentStore>>> {
            if let Some(reader) = reader.as_mut() {
                match reader.lazy(id)? {
                    // A staged row is evaluated fields, which no retained source supplies.
                    LazyChange::Staged(_) | LazyChange::Changed(false) => return Ok(None),
                    LazyChange::Changed(true) => {
                        return Ok(self.identities.as_ref().map(|layer| layer.source()));
                    }
                    LazyChange::Unchanged => {}
                }
            }
            Ok(self.selected(id).and_then(Change::retained_source))
        };
        let Some(source) = retained(ids[start])? else {
            return Ok(None);
        };
        let mut end = start + 1;
        while end < ids.len() && retained(ids[end])?.is_some_and(|next| Arc::ptr_eq(source, next)) {
            end += 1;
        }
        Ok(Some((end, source)))
    }

    /// The newest change of `id`.
    pub(super) fn get(&self, id: DocId) -> StorageBackendResult<Option<ChangeRef<'_>>> {
        if let Some(change) = self.staged_change(id)? {
            return Ok(Some(ChangeRef::Owned(change)));
        }
        if let (Some(present), Some(layer)) = (self.identity_presence(id)?, &self.identities) {
            return Ok(Some(ChangeRef::Owned(layer.change(present))));
        }
        Ok(self.selected(id).map(ChangeRef::Borrowed))
    }

    /// Visit the changes after `after` in identity order, each with whether its row is present afterwards.
    pub fn changes_after(&self, after: Option<DocId>) -> Changes<bool, bool> {
        Changes::new(
            self,
            after,
            ChangeKind {
                from_selection: Change::present,
                from_identity: |_, present| present,
                from_staged: std::convert::identity,
                cursor: StagedRowsView::presence,
            },
        )
    }

    /// Visit the changes after `after` in identity order.
    pub(super) fn change_rows_after(&self, after: Option<DocId>) -> Changes<StagedRow, Change> {
        Changes::new(
            self,
            after,
            ChangeKind {
                from_selection: Change::clone,
                from_identity: IdentityLayer::change,
                from_staged: staged_change,
                cursor: StagedRowsView::rows,
            },
        )
    }
}

/// What a merged iteration yields from each layer.
struct ChangeKind<S, T> {
    from_selection: fn(&Change) -> T,
    from_identity: fn(&IdentityLayer, bool) -> T,
    from_staged: fn(S) -> T,
    cursor: fn(&StagedRowsView, Option<DocId>) -> StagedCursor<S>,
}

/// The changes of a selection and of the lazy layers above it, in identity order. The newest change of an identity shadows the older ones. The iterator shares the changes it reads, so it outlives the reader that created it.
pub struct Changes<S, T> {
    changes: DocumentChanges,
    /// The position of the next selected change.
    selected: usize,
    from_selection: fn(&Change) -> T,
    from_identity: fn(&IdentityLayer, bool) -> T,
    from_staged: fn(S) -> T,
    identity: Option<IdentityHead>,
    /// Each command's staged rows, the oldest first.
    staged: Vec<StagedHead<S>>,
    failed: bool,
}

/// One command's staged rows and the next of them.
struct StagedHead<S> {
    cursor: StagedCursor<S>,
    next: Option<(DocId, S)>,
    done: bool,
}

impl<S: Default, T> Changes<S, T> {
    fn new(changes: &DocumentChanges, after: Option<DocId>, kind: ChangeKind<S, T>) -> Self {
        let selected = changes
            .rows()
            .partition_point(|(id, _)| after.is_some_and(|after| *id <= after));
        Self {
            changes: changes.clone(),
            selected,
            from_selection: kind.from_selection,
            from_identity: kind.from_identity,
            from_staged: kind.from_staged,
            identity: changes.identities.as_ref().map(|layer| IdentityHead {
                cursor: layer.view.cursor(after),
                next: None,
                done: false,
            }),
            staged: changes
                .staged_views()
                .iter()
                .map(|view| StagedHead {
                    cursor: (kind.cursor)(view, after),
                    next: None,
                    done: false,
                })
                .collect(),
            failed: false,
        }
    }

    fn advance(&mut self) -> StorageBackendResult<Option<(DocId, T)>> {
        if let Some(staged) = &self.changes.staged {
            for head in &mut self.staged {
                if head.next.is_none() && !head.done {
                    head.next = head.cursor.next(&staged.control)?;
                    head.done = head.next.is_none();
                }
            }
        }
        if let (Some(head), Some(layer)) = (&mut self.identity, &self.changes.identities) {
            if head.next.is_none() && !head.done {
                head.next = head.cursor.next(&layer.control)?;
                head.done = head.next.is_none();
            }
        }
        let rows = self.changes.rows();
        let selected = rows.get(self.selected).map(|(id, _)| *id);
        let changed = self
            .identity
            .as_ref()
            .and_then(|head| head.next.map(|(id, _)| id));
        let Some(least) = self
            .staged
            .iter()
            .filter_map(|head| head.next.as_ref().map(|(id, _)| *id))
            .chain(changed)
            .chain(selected)
            .min()
        else {
            return Ok(None);
        };
        let mut found = None;
        for head in self.staged.iter_mut().rev() {
            if head.next.as_ref().is_some_and(|(id, _)| *id == least) {
                let (id, row) = head.next.take().expect("a peeked staged row");
                if found.is_none() {
                    found = Some((id, (self.from_staged)(row)));
                }
            }
        }
        if changed == Some(least) {
            let head = self.identity.as_mut().expect("a peeked identity");
            let (id, present) = head.next.take().expect("a peeked identity");
            if found.is_none() {
                let layer = self.changes.identities.as_ref().expect("an identity layer");
                found = Some((id, (self.from_identity)(layer, present)));
            }
        }
        if selected == Some(least) {
            let (id, change) = &rows[self.selected];
            self.selected += 1;
            if found.is_none() {
                found = Some((*id, (self.from_selection)(change)));
            }
        }
        Ok(found)
    }
}

impl<S: Default, T> Iterator for Changes<S, T> {
    type Item = StorageBackendResult<(DocId, T)>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        match self.advance() {
            Ok(change) => change.map(Ok),
            Err(error) => {
                self.failed = true;
                Some(Err(error))
            }
        }
    }
}
