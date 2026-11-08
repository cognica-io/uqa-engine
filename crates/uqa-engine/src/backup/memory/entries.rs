//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Select retained Engine owners in the storage mapping's canonical component order.

use super::{catalogs, field_analyzers, resources, table_cursor::TableCursor};
use crate::state::SnapshotCut;
use uqa_core::{memory::Budgeted, RelationIdentity};
use uqa_storage::{
    backup::{
        memory_image_namespace, BackupResult, MemoryImageFamily, MemoryImageHeader, MemoryImageKey,
        NativeImageEntries, NativeImageStep, NativeImageUnit, UnitKind,
    },
    image_io::ImageReader,
    read_control::StorageReadControl,
};

enum Stage {
    Header,
    Catalogs,
    Fields,
    Resources,
    Tables,
    Complete,
}

pub(super) struct Entries {
    source: SnapshotCut,
    header: MemoryImageHeader,
    stage: Stage,
    catalog: u16,
    field: Option<Budgeted<field_analyzers::Key>>,
    resource_kind: u8,
    resource: Option<Budgeted<String>>,
    previous_table: Option<Budgeted<RelationIdentity>>,
    table: Option<TableCursor>,
}

impl Entries {
    pub(super) fn new(source: SnapshotCut, header: MemoryImageHeader) -> Self {
        Self {
            source,
            header,
            stage: Stage::Header,
            catalog: 1,
            field: None,
            resource_kind: 1,
            resource: None,
            previous_table: None,
            table: None,
        }
    }

    fn field(&mut self, control: &StorageReadControl) -> BackupResult<NativeImageStep> {
        let Some((key, reader)) =
            field_analyzers::next(&self.source, self.field.as_deref(), control)?
        else {
            self.field = None;
            self.stage = Stage::Resources;
            return Ok(NativeImageStep::Pending);
        };
        let address = MemoryImageKey::field_analyzer(&key.0, &key.1, control)?;
        self.field = Some(key);
        Ok(unit(
            MemoryImageFamily::FieldAnalyzer,
            address,
            UnitKind::Resource,
            reader,
        ))
    }

    fn resource(&mut self, control: &StorageReadControl) -> BackupResult<NativeImageStep> {
        let next = resources::next(
            &self.source,
            self.resource_kind,
            self.resource.as_ref().map(|name| name.as_str()),
            control,
        )?;
        let Some((name, reader)) = next else {
            self.resource = None;
            self.resource_kind += 1;
            if self.resource_kind > resources::RESOURCE_KINDS {
                self.stage = Stage::Tables;
            }
            return Ok(NativeImageStep::Pending);
        };
        let key = MemoryImageKey::resource(self.resource_kind, &name, control)?;
        self.resource = Some(name);
        Ok(unit(
            MemoryImageFamily::Resource,
            key,
            UnitKind::Resource,
            reader,
        ))
    }

    fn table(&mut self, control: &StorageReadControl) -> BackupResult<NativeImageStep> {
        if let Some(table) = &mut self.table {
            let step = table.next(&self.source.cut, control)?;
            if matches!(step, NativeImageStep::Complete) {
                self.previous_table = self.table.take().map(TableCursor::into_name);
                return Ok(NativeImageStep::Pending);
            }
            return Ok(step);
        }
        let Some((name, table)) = self.source.next_table(self.previous_table.as_deref())? else {
            self.stage = Stage::Complete;
            self.previous_table = None;
            return Ok(NativeImageStep::Complete);
        };
        self.table = Some(TableCursor::new(name, table, control)?);
        self.previous_table = None;
        Ok(NativeImageStep::Pending)
    }
}

impl NativeImageEntries for Entries {
    fn check(&self) -> BackupResult<()> {
        self.source.cut.check().map_err(Into::into)
    }

    fn next_image(&mut self, control: &StorageReadControl) -> BackupResult<NativeImageStep> {
        match self.stage {
            Stage::Header => {
                let reader = self.header.image_reader(control)?;
                let key = MemoryImageKey::empty(control)?;
                self.stage = Stage::Catalogs;
                Ok(unit(
                    MemoryImageFamily::Header,
                    key,
                    UnitKind::Metadata,
                    reader,
                ))
            }
            Stage::Catalogs => {
                if self.catalog > catalogs::CATALOG_COUNT {
                    self.stage = Stage::Fields;
                    return Ok(NativeImageStep::Pending);
                }
                let key = MemoryImageKey::catalog(self.catalog, control)?;
                let reader = catalogs::image(&self.source, self.catalog, control)?
                    .expect("known catalog slot");
                self.catalog += 1;
                Ok(unit(
                    MemoryImageFamily::Catalog,
                    key,
                    UnitKind::Metadata,
                    reader,
                ))
            }
            Stage::Fields => self.field(control),
            Stage::Resources => self.resource(control),
            Stage::Tables => self.table(control),
            Stage::Complete => Ok(NativeImageStep::Complete),
        }
    }
}

pub(super) fn unit(
    family: MemoryImageFamily,
    key: MemoryImageKey,
    kind: UnitKind,
    reader: ImageReader<'static>,
) -> NativeImageStep {
    NativeImageStep::Unit(NativeImageUnit {
        namespace: memory_image_namespace(),
        generation: 0,
        family: family as u16,
        key: key.into_bytes(),
        kind,
        reader,
    })
}
