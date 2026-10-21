//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Freeze complete candidate support in identity order before any row evaluation.

use crate::{
    mutation::PhysicalRowBuffer,
    physical::physical_exec_error,
    spill::{SharedSpillReader, SpillBatchWriter, SpillRows},
    PhysicalRow, RowSchema, SharedSpill, SpillBuffer,
};
use std::sync::Arc;
use uqa_core::{CancellationToken, DocId, Value};
use uqa_sql::SQLError;

#[derive(Clone)]
enum IdentityStorage {
    Memory(Arc<[DocId]>),
    Spill(SharedSpill),
}

/// An immutable, ascending, duplicate-free candidate set. Independent cursors share its captured source, with bounded resident state.
#[derive(Clone)]
pub struct SelectedIds {
    storage: IdentityStorage,
    cancellation: CancellationToken,
}

impl SelectedIds {
    /// Classify the complete ordered input once before either output is read. `true` goes to the first set, `false` to the second, and `None` omits the identity.
    pub(crate) fn partition(
        &self,
        bytes: usize,
        mut select: impl FnMut(DocId) -> Result<Option<bool>, SQLError>,
    ) -> Result<(Self, Self), SQLError> {
        let mut first = IdentityBuilder::new(bytes / 2, &self.cancellation);
        let mut second = IdentityBuilder::new(bytes / 2, &self.cancellation);
        for id in self.cursor() {
            let id = id?;
            match select(id)? {
                Some(true) => first.push(id)?,
                Some(false) => second.push(id)?,
                None => {}
            }
        }
        Ok((first.finish(false)?, second.finish(false)?))
    }

    /// Freeze a callback-based producer before any consumer reads its identities.
    pub(crate) fn capture_visit(
        bytes: usize,
        cancellation: &CancellationToken,
        produce: impl FnOnce(&mut dyn FnMut(DocId) -> Result<(), SQLError>) -> Result<(), SQLError>,
    ) -> Result<Self, SQLError> {
        let mut builder = IdentityBuilder::new(bytes, cancellation);
        produce(&mut |id| builder.push(id))?;
        builder.finish(true)
    }

    /// Capture all identities before consumption, using bounded memory and ascending, duplicate-free output.
    pub fn capture(
        ids: impl IntoIterator<Item = Result<DocId, SQLError>>,
        bytes: usize,
        cancellation: &CancellationToken,
    ) -> Result<Self, SQLError> {
        let mut builder = IdentityBuilder::new(bytes, cancellation);
        for id in ids {
            builder.push(id?)?;
        }
        builder.finish(true)
    }

    pub(crate) fn from_sorted(
        ids: Vec<DocId>,
        bytes: usize,
        cancellation: &CancellationToken,
    ) -> Result<Self, SQLError> {
        cancellation.check()?;
        if ids.len() <= bytes / size_of::<DocId>() {
            return Ok(Self {
                storage: IdentityStorage::Memory(ids.into()),
                cancellation: cancellation.clone(),
            });
        }
        let mut builder = IdentityBuilder::new(bytes, cancellation);
        for id in ids {
            builder.push(id)?;
        }
        builder.finish(false)
    }

    pub fn len(&self) -> usize {
        match &self.storage {
            IdentityStorage::Memory(ids) => ids.len(),
            IdentityStorage::Spill(rows) => rows.rows(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn cursor(&self) -> SelectedIdCursor {
        SelectedIdCursor {
            source: self.clone(),
            position: 0,
            rows: None,
            failed: false,
        }
    }
}

pub struct SelectedIdCursor {
    source: SelectedIds,
    position: usize,
    rows: Option<SpillRows<SharedSpillReader>>,
    failed: bool,
}

impl SelectedIdCursor {
    fn read(&mut self) -> Result<Option<DocId>, SQLError> {
        self.source.cancellation.check()?;
        let id = match &self.source.storage {
            IdentityStorage::Memory(ids) => ids.get(self.position).copied(),
            IdentityStorage::Spill(source) => {
                if self.rows.is_none() {
                    self.rows = Some(source.read_rows().map_err(physical_exec_error)?);
                }
                self.rows
                    .as_mut()
                    .expect("opened candidate cursor")
                    .next()
                    .map(|row| decode(row.map_err(physical_exec_error)?.row))
                    .transpose()?
            }
        };
        self.position += usize::from(id.is_some());
        Ok(id)
    }
}

impl Iterator for SelectedIdCursor {
    type Item = Result<DocId, SQLError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.position == self.source.len() {
            return None;
        }
        match self.read() {
            Ok(Some(id)) => Some(Ok(id)),
            Ok(None) => {
                self.failed = true;
                Some(Err(SQLError::Internal(
                    "truncated candidate identities".into(),
                )))
            }
            Err(error) => {
                self.failed = true;
                Some(Err(error))
            }
        }
    }
}

struct SpilledIds {
    rows: SpillBuffer,
    writer: SpillBatchWriter,
    schema: RowSchema,
}

enum CapturedIds {
    Memory(Vec<DocId>),
    Spill(Box<SpilledIds>),
}

pub(super) struct IdentityBuilder {
    records: CapturedIds,
    bytes: usize,
    cancellation: CancellationToken,
    previous: Option<DocId>,
    ascending: bool,
}

impl IdentityBuilder {
    pub(super) fn new(bytes: usize, cancellation: &CancellationToken) -> Self {
        Self {
            records: CapturedIds::Memory(Vec::new()),
            bytes,
            cancellation: cancellation.clone(),
            previous: None,
            ascending: true,
        }
    }

    pub(super) fn push(&mut self, id: DocId) -> Result<(), SQLError> {
        self.cancellation.check()?;
        self.ascending &= self.previous.is_none_or(|previous| previous < id);
        self.previous = Some(id);
        if let CapturedIds::Memory(ids) = &mut self.records {
            let limit = self.bytes / 4 / size_of::<DocId>();
            if ids.len() < limit {
                if ids.len() == ids.capacity() {
                    let capacity = ids.capacity().saturating_mul(2).max(4).min(limit);
                    ids.reserve_exact(capacity - ids.len());
                }
                ids.push(id);
                return Ok(());
            }
            let schema = RowSchema::with_internal_relation_types(
                uqa_sql::ast::InternalRelationId::allocate(),
                vec![None],
            );
            let mut spilled = SpilledIds {
                rows: SpillBuffer::new(self.bytes / 4),
                writer: SpillBatchWriter::new(schema.clone()).map_err(physical_exec_error)?,
                schema,
            };
            for &id in ids.iter() {
                self.cancellation.check()?;
                spilled.push(id)?;
            }
            self.records = CapturedIds::Spill(Box::new(spilled));
        }
        let CapturedIds::Spill(records) = &mut self.records else {
            unreachable!("spilled candidate identities")
        };
        records.push(id)
    }

    pub(super) fn finish(self, sort: bool) -> Result<SelectedIds, SQLError> {
        self.cancellation.check()?;
        let SpilledIds {
            mut rows,
            writer,
            schema,
        } = match self.records {
            CapturedIds::Memory(mut ids) => {
                if sort && !self.ascending {
                    ids.sort_unstable();
                    ids.dedup();
                }
                return Ok(SelectedIds {
                    storage: IdentityStorage::Memory(ids.into()),
                    cancellation: self.cancellation,
                });
            }
            CapturedIds::Spill(records) => *records,
        };
        writer.finish(&mut rows).map_err(physical_exec_error)?;
        if !sort || self.ascending {
            return Ok(SelectedIds {
                storage: IdentityStorage::Spill(
                    rows.into_shared(schema).map_err(physical_exec_error)?,
                ),
                cancellation: self.cancellation,
            });
        }
        // The existing storage-backed physical key set sorts identities directly, without decorating each ID as a SQL sort row.
        let mut ordered = PhysicalRowBuffer::new(
            self.bytes / 4,
            self.cancellation.clone(),
            "sort indexed candidates",
        );
        for row in rows.drain_rows().map_err(physical_exec_error)? {
            ordered.insert("", decode(row.map_err(physical_exec_error)?.row)?)?;
        }
        let ordered = ordered.finish()?;
        let mut output = Self::new(self.bytes, &self.cancellation);
        ordered.visit(&mut |_, id| {
            output.push(id)?;
            Ok(true)
        })?;
        output.finish(false)
    }
}

impl SpilledIds {
    fn push(&mut self, id: DocId) -> Result<(), SQLError> {
        self.writer
            .push(
                &mut self.rows,
                PhysicalRow::from_values(vec![Value::Bytes(id.to_be_bytes().to_vec())]),
            )
            .map_err(physical_exec_error)
    }
}

fn decode(row: PhysicalRow) -> Result<DocId, SQLError> {
    let Some(Value::Bytes(bytes)) = row.value(0) else {
        return Err(SQLError::Internal("invalid candidate identity".into()));
    };
    Ok(u64::from_be_bytes(bytes.as_slice().try_into().map_err(
        |_| SQLError::Internal("invalid candidate identity width".into()),
    )?))
}
