//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered repository input assembles unpublished native Engine state without evaluating SQL.

use super::{
    catalogs::{CatalogInput, Destination},
    input::{TableDestination, TableInput},
    resources::ResourcesInput,
};
use crate::state::{DurableCatalogState, StorageContext, TableRegistryInput};
use std::{io::Read, sync::Arc};
use uqa_analysis::AnalyzerResources;
use uqa_core::memory::{Budgeted, MemoryBudget};
use uqa_storage::{
    backup::{
        BackupError, BackupResult, BackupSourceStep, DecodedMemoryImageAddress,
        DecodedMemoryTableUnit, MemoryImageHeader, MemoryImageImportReader,
    },
    diskann_index::build::DiskANNTemporaryBudget,
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};

/// These owners are decoded but not yet a logically validated or activated database.
pub(super) struct MemoryState {
    pub storage: StorageContext,
    pub durable: DurableCatalogState,
    pub header: MemoryImageHeader,
}

pub(super) struct Input<'a> {
    image: Option<MemoryImageImportReader<'a>>,
    candidate: Option<Budgeted<Box<Candidate>>>,
    resources: AnalyzerResources,
    temporary: DiskANNTemporaryBudget,
    maximum_value_bytes: usize,
    control: StorageReadControl,
}

struct Candidate {
    storage: StorageContext,
    durable: DurableCatalogState,
    catalogs: Option<CatalogInput>,
    resources: ResourcesInput,
    tables: TableRegistryInput,
    table: Option<TableInput>,
    mutations: MemoryBudget,
}

impl<'a> Input<'a> {
    pub(super) fn new(
        image: MemoryImageImportReader<'a>,
        resources: AnalyzerResources,
        temporary: DiskANNTemporaryBudget,
        maximum_value_bytes: usize,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        control.check()?;
        if maximum_value_bytes == 0 {
            return Err(BackupError::Invalid("zero native image value limit"));
        }
        let memory = control.memory().reserve(size_of::<Candidate>())?;
        let storage = StorageContext::memory();
        let tables = TableRegistryInput::new(
            storage
                .memory_capture
                .as_ref()
                .expect("private memory destination"),
            control,
        )?;
        let candidate = Candidate {
            storage,
            durable: DurableCatalogState::new(),
            catalogs: Some(CatalogInput::new()),
            resources: ResourcesInput::new(control.memory()),
            tables,
            table: None,
            mutations: MemoryBudget::new(usize::MAX),
        };
        control.check()?;
        Ok(Self {
            image: Some(image),
            candidate: Some(Budgeted::new(Box::new(candidate), memory)),
            resources,
            temporary,
            maximum_value_bytes,
            control: control.clone(),
        })
    }

    /// One repository verification step or one complete native component. Native codecs poll their own cancellation and allocation bounds; this is not a byte-bounded public restore job.
    pub(super) fn advance(
        &mut self,
        maximum_object_bytes: usize,
    ) -> BackupResult<Option<MemoryState>> {
        let candidate = self.candidate.take().ok_or(BackupError::Finished)?;
        let mut image = self.image.take().ok_or(BackupError::Finished)?;
        self.control.check()?;
        let (candidate, memory) = candidate.into_parts();
        let candidate = match image.advance(maximum_object_bytes)? {
            BackupSourceStep::Pending => candidate,
            BackupSourceStep::Unit => image.decode_current(|unit, reader| {
                candidate
                    .read(
                        unit.address,
                        reader,
                        &self.resources,
                        &self.temporary,
                        self.maximum_value_bytes,
                        &self.control,
                    )
                    .map(Box::new)
                    .map_err(Into::into)
            })?,
            BackupSourceStep::Complete => {
                let header = image
                    .header()
                    .ok_or(BackupError::Invalid("missing native image header"))?;
                return Ok(Some(candidate.finish(header, &self.control)?));
            }
        };
        self.candidate = Some(Budgeted::new(candidate, memory));
        self.image = Some(image);
        Ok(None)
    }
}

impl Candidate {
    fn read(
        mut self,
        address: &DecodedMemoryImageAddress,
        reader: &mut dyn Read,
        resources: &AnalyzerResources,
        temporary: &DiskANNTemporaryBudget,
        maximum_value_bytes: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        let registry = self
            .storage
            .memory_capture
            .as_ref()
            .expect("private memory destination");
        if let DecodedMemoryImageAddress::Catalog(slot) = address {
            self.catalogs = Some(self.catalogs.take().ok_or_else(invalid)?.read(
                &mut self.durable,
                *slot,
                reader,
                &Destination {
                    registry,
                    mutations: &self.mutations,
                    control,
                },
            )?);
            return Ok(self);
        }
        if let Some(catalogs) = self.catalogs.take() {
            catalogs.finish()?;
        }
        match address {
            DecodedMemoryImageAddress::FieldAnalyzer { .. }
            | DecodedMemoryImageAddress::Resource { .. } => {
                self.resources =
                    self.resources
                        .read(address, reader, resources, &self.mutations, control)?;
            }
            DecodedMemoryImageAddress::Table { unit, .. } => {
                if matches!(unit, DecodedMemoryTableUnit::Metadata(0)) {
                    if let Some(table) = self.table.take() {
                        let (name, table) = table.finish(&self.storage, control)?;
                        self.tables = self.tables.add_table(name, table)?;
                    }
                    self.table = Some(TableInput::open(address, reader, control)?);
                } else {
                    self.table = Some(self.table.take().ok_or_else(invalid)?.read(
                        unit,
                        reader,
                        &TableDestination {
                            state: &self.storage,
                            fields: &self.resources.fields,
                            resources,
                            maximum_value_bytes,
                            temporary,
                        },
                        control,
                    )?);
                }
            }
            _ => return Err(invalid()),
        }
        Ok(self)
    }

    fn finish(
        mut self,
        header: MemoryImageHeader,
        control: &StorageReadControl,
    ) -> StorageBackendResult<MemoryState> {
        if let Some(catalogs) = self.catalogs {
            catalogs.finish()?;
        }
        if let Some(table) = self.table {
            let (name, table) = table.finish(&self.storage, control)?;
            self.tables = self.tables.add_table(name, table)?;
        }
        self.resources.finish(
            &mut self.durable,
            self.storage
                .memory_capture
                .as_ref()
                .expect("private memory destination"),
            &self.mutations,
            control,
        )?;
        self.storage.tables = Arc::new(self.tables.finish()?);
        control.check()?;
        Ok(MemoryState {
            storage: self.storage,
            durable: self.durable,
            header,
        })
    }
}

fn invalid() -> StorageBackendError {
    StorageBackendError::Other("unexpected native database component".into())
}

#[cfg(test)]
mod tests;
