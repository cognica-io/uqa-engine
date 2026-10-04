//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rows that running commands staged, above a selection of changes. Each command's rows shadow the selection and the rows of the commands before it. The rows stay in the commands' own tiers, so taking them into a read copies nothing.

use super::{
    Arc, Change, DocId, DocumentChanges, DocumentStore, StorageBackendResult, StorageReadControl,
};
use crate::mutation::overlay::{StagedCursor, StagedRow, StagedRowsView};
use uqa_core::memory::BudgetedVec;

pub(super) struct StagedLayers {
    /// The commands' rows, the oldest command first.
    views: BudgetedVec<StagedRowsView>,
    /// The allowance that reads decode spilled rows into.
    pub(super) control: StorageReadControl,
}

/// Reads the changes the commands staged for the identities of one batch, in the batch's order. Ascending identities advance each command's rows once over the batch, holding one bounded page of each; other batches look each identity up.
pub(super) struct StagedReader<'a> {
    changes: &'a DocumentChanges,
    control: &'a StorageReadControl,
    ascending: bool,
    /// Each command's staged rows, the oldest first.
    heads: Vec<StagedHead<StagedRow>>,
    /// The identity read last and its staged change, which a lookahead and the read that follows it share.
    last: Option<(DocId, Option<Change>)>,
}

impl StagedReader<'_> {
    /// The newest change the commands staged for `id`.
    fn staged(&mut self, id: DocId) -> StorageBackendResult<Option<Change>> {
        if let Some((last, change)) = &self.last {
            if *last == id {
                return Ok(change.clone());
            }
        }
        let found = if self.ascending {
            let mut found = None;
            for head in self.heads.iter_mut().rev() {
                while !head.done && head.next.as_ref().is_none_or(|(next, _)| *next < id) {
                    head.next = head.cursor.next(self.control)?;
                    head.done = head.next.is_none();
                }
                if head.next.as_ref().is_some_and(|(next, _)| *next == id) {
                    let (_, row) = head.next.take().expect("a matched staged row");
                    if found.is_none() {
                        found = Some(staged_change(row));
                    }
                }
            }
            found
        } else {
            self.changes.staged_change(id)?
        };
        self.last = Some((id, found.clone()));
        Ok(found)
    }
}

/// A change read from a selection, or decoded from a command's staged rows.
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

    pub(super) fn staged_views(&self) -> &[StagedRowsView] {
        self.staged.as_ref().map_or(&[], |staged| &staged.views)
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

    /// A reader of the changes the commands staged for the identities of the batch `ids`, or `None` when no command staged rows.
    pub(super) fn staged_reader(&self, ids: &[DocId]) -> Option<StagedReader<'_>> {
        let staged = self.staged.as_ref()?;
        let ascending = ids.len() > 1 && ids.windows(2).all(|pair| pair[0] < pair[1]);
        let heads = if ascending {
            staged
                .views
                .iter()
                .map(|view| StagedHead {
                    cursor: view.rows(ids[0].checked_sub(1)),
                    next: None,
                    done: false,
                })
                .collect()
        } else {
            Vec::new()
        };
        Some(StagedReader {
            changes: self,
            control: &staged.control,
            ascending,
            heads,
            last: None,
        })
    }

    /// The newest change of `id`, an identity of the batch that `reader` reads.
    pub(super) fn batch_change<'a>(
        &'a self,
        reader: &mut Option<StagedReader<'a>>,
        id: DocId,
    ) -> StorageBackendResult<Option<ChangeRef<'a>>> {
        if let Some(reader) = reader {
            if let Some(change) = reader.staged(id)? {
                return Ok(Some(ChangeRef::Owned(change)));
            }
        }
        Ok(self.selected(id).map(ChangeRef::Borrowed))
    }

    /// The end of the run of `ids` from `start` whose rows one retained source supplies, with that source; `reader` reads the batch's staged changes.
    pub(super) fn batch_source_run<'a>(
        &'a self,
        ids: &[DocId],
        start: usize,
        reader: &mut Option<StagedReader<'a>>,
    ) -> StorageBackendResult<Option<(usize, &'a Arc<dyn DocumentStore>)>> {
        // A staged row is evaluated fields, which no retained source supplies.
        let mut retained = |id: DocId| -> StorageBackendResult<Option<&'a Arc<dyn DocumentStore>>> {
            if let Some(reader) = reader.as_mut() {
                if reader.staged(id)?.is_some() {
                    return Ok(None);
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
        Ok(self.selected(id).map(ChangeRef::Borrowed))
    }

    /// Visit the changes after `after` in identity order, each with whether its row is present afterwards.
    pub fn changes_after(&self, after: Option<DocId>) -> Changes<bool, bool> {
        Changes::new(
            self,
            after,
            Change::present,
            std::convert::identity,
            StagedRowsView::presence,
        )
    }

    /// Visit the changes after `after` in identity order.
    pub(super) fn change_rows_after(&self, after: Option<DocId>) -> Changes<StagedRow, Change> {
        Changes::new(
            self,
            after,
            Change::clone,
            staged_change,
            StagedRowsView::rows,
        )
    }
}

/// The changes of a selection and of the commands' staged rows above it, in identity order. The newest change of an identity shadows the older ones. The iterator shares the changes it reads, so it outlives the reader that created it.
pub struct Changes<S, T> {
    changes: DocumentChanges,
    /// The position of the next selected change.
    selected: usize,
    from_selection: fn(&Change) -> T,
    from_staged: fn(S) -> T,
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
    fn new(
        changes: &DocumentChanges,
        after: Option<DocId>,
        from_selection: fn(&Change) -> T,
        from_staged: fn(S) -> T,
        cursor: fn(&StagedRowsView, Option<DocId>) -> StagedCursor<S>,
    ) -> Self {
        let selected = changes
            .rows()
            .partition_point(|(id, _)| after.is_some_and(|after| *id <= after));
        Self {
            changes: changes.clone(),
            selected,
            from_selection,
            from_staged,
            staged: changes
                .staged_views()
                .iter()
                .map(|view| StagedHead {
                    cursor: cursor(view, after),
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
        let rows = self.changes.rows();
        let selected = rows.get(self.selected).map(|(id, _)| *id);
        let Some(least) = self
            .staged
            .iter()
            .filter_map(|head| head.next.as_ref().map(|(id, _)| *id))
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
