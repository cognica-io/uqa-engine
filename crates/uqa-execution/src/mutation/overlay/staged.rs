//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The rows one command staged for one table, in two tiers: the newest in a persistent ordered root in memory, older ones spilled to encrypted temporary files once the command's allowance is under pressure. A row in memory shadows its spilled version. Reads take an immutable view of both tiers, so a read sees the rows staged before it without copying them.

use std::ops::Bound;
use std::sync::Arc;

use uqa_core::memory::{BudgetedSharedMap, BudgetedVec};
use uqa_core::DocId;
use uqa_storage::mvcc::PrivateRecordSnapshot;
use uqa_storage::read_control::StorageReadControl;
use uqa_storage::StorageBackendResult;

use super::spilled::{self, RowPage, SpilledRows, StagedRow};

/// The resident bytes of a row in memory beyond its fields, as an estimate: its tree node and entry, their shared handles and allocation headers.
const ROW_OVERHEAD: usize = 192;
/// The memory tier spills once the allowance is more than this fraction used.
const PRESSURE_DIVISOR: usize = 2;
/// The memory tier spills only once it holds at least this fraction of the allowance, so that memory held elsewhere does not spill a run for every row.
const RESIDENT_DIVISOR: usize = 16;

/// The memory tier of staged rows.
pub(super) type MemoryRows = BudgetedSharedMap<DocId, StagedRow>;

#[derive(Clone)]
pub(super) struct StagedRows {
    pub(super) memory: MemoryRows,
    /// The estimated bytes the memory tier holds, which decide when it spills.
    resident: usize,
    pub(super) spilled: Option<SpilledRows>,
    /// The rows moved into the spilled tier, counting a row once for each move.
    spilled_rows: u64,
}

impl StagedRows {
    pub(super) fn new(control: &StorageReadControl) -> Self {
        Self {
            memory: MemoryRows::new(control.memory()),
            resident: 0,
            spilled: None,
            spilled_rows: 0,
        }
    }

    /// At least the number of rows staged, without reading them: a row staged again after it spilled counts twice.
    pub(super) fn count_bound(&self) -> u64 {
        self.spilled_rows
            .saturating_add(u64::try_from(self.memory.len()).unwrap_or(u64::MAX))
    }

    pub(super) fn is_empty(&self) -> bool {
        self.memory.is_empty() && self.spilled.is_none()
    }

    /// Whether the memory tier should move into the spilled tier before another row is staged: the allowance is more than half used and this tier holds a share of it worth a run.
    pub(super) fn needs_room(&self, control: &StorageReadControl) -> bool {
        let budget = control.memory();
        !self.memory.is_empty()
            && self.resident >= budget.limit() / RESIDENT_DIVISOR
            && budget.used() > budget.limit() / PRESSURE_DIVISOR
    }

    /// Release the memory tier once every row of it is in the spilled tier.
    pub(super) fn clear_memory(&mut self) {
        self.spilled_rows = self
            .spilled_rows
            .saturating_add(u64::try_from(self.memory.len()).unwrap_or(u64::MAX));
        self.memory = MemoryRows::new(self.memory.budget());
        self.resident = 0;
    }

    /// Stage `row` for `id` in the memory tier. Failure leaves both tiers unchanged.
    pub(super) fn insert(&mut self, id: DocId, row: StagedRow) -> StorageBackendResult<()> {
        let bytes = row.as_ref().map_or(Ok(0), |row| {
            crate::spill::encoded_document_size(row.fields.as_ref())
                .map_err(|error| uqa_storage::StorageBackendError::Other(error.to_string()))
        })?;
        self.memory.try_insert(id, row)?;
        self.resident = self
            .resident
            .saturating_add(bytes)
            .saturating_add(ROW_OVERHEAD);
        Ok(())
    }

    /// The row staged for `id`, if any.
    pub(super) fn get(
        &self,
        id: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<StagedRow>> {
        if let Some(row) = self.memory.get(&id) {
            return Ok(Some(row.clone()));
        }
        match &self.spilled {
            Some(spilled) => spilled::row(spilled.view(), id, control),
            None => Ok(None),
        }
    }

    /// Whether a row is staged for `id`, without decoding a spilled row.
    pub(super) fn contains(
        &self,
        id: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<bool> {
        if self.memory.get(&id).is_some() {
            return Ok(true);
        }
        match &self.spilled {
            Some(spilled) => spilled::holds_row(spilled.view(), id, control),
            None => Ok(false),
        }
    }

    /// An immutable view of the rows staged so far, which later stages do not change.
    pub(crate) fn view(&self) -> StagedRowsView {
        StagedRowsView {
            memory: self.memory.clone(),
            spilled: self
                .spilled
                .as_ref()
                .map(|spilled| Arc::clone(spilled.view())),
        }
    }
}

/// An immutable view of the rows one command staged for one table.
#[derive(Clone)]
pub(crate) struct StagedRowsView {
    memory: MemoryRows,
    spilled: Option<Arc<PrivateRecordSnapshot>>,
}

impl StagedRowsView {
    /// The row staged for `id`, if any.
    pub(crate) fn get(
        &self,
        id: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<StagedRow>> {
        if let Some(row) = self.memory.get(&id) {
            return Ok(Some(row.clone()));
        }
        match &self.spilled {
            Some(spilled) => spilled::row(spilled, id, control),
            None => Ok(None),
        }
    }

    /// Whether a row is staged for `id`, without decoding a spilled row.
    pub(crate) fn contains(
        &self,
        id: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<bool> {
        if self.memory.get(&id).is_some() {
            return Ok(true);
        }
        match &self.spilled {
            Some(spilled) => spilled::holds_row(spilled, id, control),
            None => Ok(false),
        }
    }

    /// Visit the staged rows after identity `after` in identity order.
    pub(crate) fn rows(&self, after: Option<DocId>) -> StagedCursor<StagedRow> {
        StagedCursor::new(
            self,
            after,
            |row: &StagedRow| row.clone(),
            spilled::row_page,
        )
    }

    /// Visit the identities of the staged rows after identity `after` in identity order, each with whether its row is present.
    pub(crate) fn presence(&self, after: Option<DocId>) -> StagedCursor<bool> {
        StagedCursor::new(
            self,
            after,
            |row: &StagedRow| row.is_some(),
            spilled::presence_page,
        )
    }
}

type PageReader<T> = fn(
    &PrivateRecordSnapshot,
    Option<DocId>,
    &StorageReadControl,
) -> StorageBackendResult<RowPage<T>>;

/// The staged rows of a view in identity order, merging the memory tier with bounded pages of the spilled tier; a row in memory shadows its spilled version. The cursor owns its view, so it outlives the reader that created it.
pub(crate) struct StagedCursor<T> {
    view: StagedRowsView,
    from_memory: fn(&StagedRow) -> T,
    read_page: PageReader<T>,
    /// The last identity the memory tier supplied, or the identity the cursor starts after.
    memory_after: Option<DocId>,
    memory_done: bool,
    page: Option<BudgetedVec<(DocId, T)>>,
    position: usize,
    /// Where the next page of the spilled tier starts: after this identity, or from the start.
    resume: Option<DocId>,
    spilled_done: bool,
}

impl<T: Default> StagedCursor<T> {
    fn new(
        view: &StagedRowsView,
        after: Option<DocId>,
        from_memory: fn(&StagedRow) -> T,
        read_page: PageReader<T>,
    ) -> Self {
        Self {
            view: view.clone(),
            from_memory,
            read_page,
            memory_after: after,
            memory_done: view.memory.is_empty(),
            page: None,
            position: 0,
            resume: after,
            spilled_done: view.spilled.is_none(),
        }
    }

    /// The identity of the next row in memory.
    fn peek_memory(&mut self) -> Option<DocId> {
        if self.memory_done {
            return None;
        }
        let start = self
            .memory_after
            .as_ref()
            .map_or(Bound::Unbounded, Bound::Excluded);
        let next = self.view.memory.range_from(start).next().map(|(id, _)| *id);
        self.memory_done = next.is_none();
        next
    }

    /// The identity of the next spilled row, reading another page when this one is used up.
    fn peek_spilled(
        &mut self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DocId>> {
        loop {
            if let Some((id, _)) = self.page.as_ref().and_then(|page| page.get(self.position)) {
                return Ok(Some(*id));
            }
            if self.spilled_done {
                return Ok(None);
            }
            let view = self
                .view
                .spilled
                .as_deref()
                .expect("an unfinished spilled tier");
            // Release the used page before reading the next one.
            self.page = None;
            let page = (self.read_page)(view, self.resume, control)?;
            self.spilled_done = page.resume.is_none();
            self.resume = page.resume;
            self.page = Some(page.rows);
            self.position = 0;
        }
    }

    fn take_spilled(&mut self) -> (DocId, T) {
        let page = self.page.as_mut().expect("a peeked page");
        let (id, row) = &mut page[self.position];
        self.position += 1;
        (*id, std::mem::take(row))
    }

    /// The next staged row in identity order.
    pub(crate) fn next(
        &mut self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<(DocId, T)>> {
        control.check()?;
        let spilled = self.peek_spilled(control)?;
        let memory = self.peek_memory();
        match (memory, spilled) {
            (None, None) => Ok(None),
            (Some(memory), spilled) if spilled.is_none_or(|spilled| memory <= spilled) => {
                if spilled == Some(memory) {
                    self.take_spilled();
                }
                self.memory_after = Some(memory);
                let row = self.view.memory.get(&memory).expect("a peeked row");
                Ok(Some((memory, (self.from_memory)(row))))
            }
            _ => Ok(Some(self.take_spilled())),
        }
    }
}
