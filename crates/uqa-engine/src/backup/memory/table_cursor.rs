//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A selected table retains one metadata, document, text, vector or evaluated-index cursor.

use super::{data, entries::unit, tables};
use crate::state::CapturedTable;
use uqa_core::{
    memory::{Budgeted, ProductionControl},
    DocId, RelationIdentity, RelationName, RelationReference,
};
use uqa_storage::{
    backup::{
        BackupResult, MemoryImageFamily, MemoryImageKey, MemoryTableUnit, NativeImageStep, UnitKind,
    },
    image_io::ImageReader,
    memory_capture::MemoryCapture,
    read_control::StorageReadControl,
    StorageBackendResult, ValueIndexKey,
};

enum Stage {
    Metadata(u8),
    Documents,
    Text,
    Vectors,
    Indexes,
    Complete,
}

pub(super) struct TableCursor {
    name: Budgeted<RelationIdentity>,
    table: tables::TableOwner,
    stage: Stage,
    document: Option<DocId>,
    field: Option<Budgeted<String>>,
    index_key: Option<Budgeted<ValueIndexKey>>,
    index: Option<data::ValueOwner>,
}

impl TableCursor {
    pub(super) fn new(
        name: &RelationIdentity,
        table: CapturedTable,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        let production = ProductionControl::new(
            control.memory(),
            control.cancellation(),
            control.cancellation(),
        );
        let name = RelationReference {
            schema: Some(RelationName::decoded(&name.schema)),
            name: RelationName::decoded(&name.name),
        }
        .identity_with_control(RelationName::decoded("public"), &production)?
        .into_budgeted()
        .expect("admitted table cursor identity");
        Ok(Self {
            name,
            table: tables::retain(table, control)?,
            stage: Stage::Metadata(0),
            document: None,
            field: None,
            index_key: None,
            index: None,
        })
    }

    pub(super) fn into_name(self) -> Budgeted<RelationIdentity> {
        self.name
    }

    fn emit(
        &self,
        component: MemoryTableUnit<'_>,
        kind: UnitKind,
        reader: ImageReader<'static>,
        control: &StorageReadControl,
    ) -> BackupResult<NativeImageStep> {
        let key = MemoryImageKey::table(
            &self.name,
            self.table.object_id,
            *self.table.storage_generation.get()?,
            component,
            control,
        )?;
        Ok(unit(MemoryImageFamily::Table, key, kind, reader))
    }

    pub(super) fn next(
        &mut self,
        cut: &MemoryCapture,
        control: &StorageReadControl,
    ) -> BackupResult<NativeImageStep> {
        match self.stage {
            Stage::Metadata(slot) => {
                if slot == tables::TABLE_METADATA_COUNT {
                    self.stage = Stage::Documents;
                    return Ok(NativeImageStep::Pending);
                }
                let reader =
                    tables::image(&self.table, slot, control)?.expect("known table metadata slot");
                self.stage = Stage::Metadata(slot + 1);
                self.emit(
                    MemoryTableUnit::Metadata(slot),
                    UnitKind::Metadata,
                    reader,
                    control,
                )
            }
            Stage::Documents | Stage::Text => self.document(control),
            Stage::Vectors => self.vector(cut, control),
            Stage::Indexes => self.index(cut, control),
            Stage::Complete => Ok(NativeImageStep::Complete),
        }
    }

    fn document(&mut self, control: &StorageReadControl) -> BackupResult<NativeImageStep> {
        let text = matches!(self.stage, Stage::Text);
        let next = if text {
            data::next_text(&self.table, self.document, control)?
        } else {
            data::next_document(&self.table, self.document, control)?
        };
        let Some((id, reader)) = next else {
            self.document = None;
            self.stage = if text { Stage::Vectors } else { Stage::Text };
            return Ok(NativeImageStep::Pending);
        };
        self.document = Some(id);
        self.emit(
            if text {
                MemoryTableUnit::Text(id)
            } else {
                MemoryTableUnit::Document(id)
            },
            UnitKind::Record,
            reader,
            control,
        )
    }

    fn vector(
        &mut self,
        cut: &MemoryCapture,
        control: &StorageReadControl,
    ) -> BackupResult<NativeImageStep> {
        let next = data::next_vector(
            &self.table,
            cut,
            self.field.as_ref().map(|field| field.as_str()),
            control,
        )?;
        let Some((field, reader)) = next else {
            self.field = None;
            self.stage = Stage::Indexes;
            return Ok(NativeImageStep::Pending);
        };
        let unit = self.emit(
            MemoryTableUnit::Vector(&field),
            UnitKind::Artifact,
            reader,
            control,
        )?;
        self.field = Some(field);
        Ok(unit)
    }

    fn index(
        &mut self,
        cut: &MemoryCapture,
        control: &StorageReadControl,
    ) -> BackupResult<NativeImageStep> {
        if let Some(index) = &self.index {
            let Some((id, present)) = index.next_image_position(self.document, control)? else {
                self.index = None;
                self.document = None;
                return Ok(NativeImageStep::Pending);
            };
            self.document = Some(id);
            if !present {
                return Ok(NativeImageStep::Pending);
            }
            let reader = data::value_index_image(index, Some(id), control)?;
            return self.emit(
                MemoryTableUnit::ValueIndex {
                    name: self.index_key.as_ref().unwrap().name(),
                    document: Some(id),
                },
                UnitKind::Record,
                reader,
                control,
            );
        }
        let next = data::next_value_index(&self.table, cut, self.index_key.as_deref(), control)?;
        let Some((key, index)) = next else {
            self.index_key = None;
            self.stage = Stage::Complete;
            return Ok(NativeImageStep::Complete);
        };
        let reader = data::value_index_image(&index, None, control)?;
        let unit = self.emit(
            MemoryTableUnit::ValueIndex {
                name: key.name(),
                document: None,
            },
            UnitKind::Metadata,
            reader,
            control,
        )?;
        self.index = Some(index);
        self.index_key = Some(key);
        Ok(unit)
    }
}
