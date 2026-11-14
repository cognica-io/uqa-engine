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
use uqa_core::{
    image_io::ImageDecodeWindow,
    memory::{Budgeted, MemoryBudget},
};
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
pub(in crate::backup) struct MemoryState {
    pub storage: StorageContext,
    pub durable: DurableCatalogState,
    pub header: MemoryImageHeader,
}

impl MemoryState {
    /// Construct an unpublished session through the ordinary runtime owner and check native authority, declared types and stored-plan relations. Complete dependency validation still precedes public activation.
    pub(in crate::backup) fn into_private_engine(
        self,
        retention: MemoryBudget,
        control: &StorageReadControl,
    ) -> BackupResult<crate::Engine> {
        control.check()?;
        let counter = uqa_execution::catalog::identity::CatalogOidCounter::from_position(
            self.header.catalog_oid_position,
        )?;
        let engine =
            crate::Engine::from_memory_state(self.storage, self.durable, counter, retention);
        crate::backup::validation::validate(&engine, control)?;
        Ok(engine)
    }
}

pub(in crate::backup) struct Input<'a> {
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

#[derive(Clone, Copy)]
struct DecodeLimits {
    value_bytes: usize,
    window: ImageDecodeWindow,
}

impl<'a> Input<'a> {
    pub(in crate::backup) fn new(
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
        let tables = TableRegistryInput::new(storage.memory_capture.as_ref(), control)?;
        let candidate = Candidate {
            storage,
            durable: DurableCatalogState::new(),
            catalogs: Some(CatalogInput::new()),
            resources: ResourcesInput::new(control.memory())?,
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

    /// One repository verification step, a catalog byte/poll window or one complete native component. Remaining component codecs and post-input validation still require resumable scheduling; this is not a fully bounded public restore job.
    pub(in crate::backup) fn advance(
        &mut self,
        maximum_object_bytes: usize,
    ) -> BackupResult<Option<MemoryState>> {
        let candidate = self.candidate.take().ok_or(BackupError::Finished)?;
        let mut image = self.image.take().ok_or(BackupError::Finished)?;
        self.control.check()?;
        let candidate = match image.advance(maximum_object_bytes)? {
            BackupSourceStep::Pending => candidate,
            BackupSourceStep::Unit => {
                let mut retained = None;
                image.decode_current_step(|unit, reader| {
                    let mut complete = false;
                    retained = Some(candidate.try_map(|candidate| {
                        candidate
                            .read(
                                unit.address,
                                reader,
                                &self.resources,
                                &self.temporary,
                                DecodeLimits {
                                    value_bytes: self.maximum_value_bytes,
                                    window: ImageDecodeWindow {
                                        bytes: maximum_object_bytes,
                                        steps: 64,
                                    },
                                },
                                &self.control,
                            )
                            .map(|(candidate, finished)| {
                                complete = finished;
                                Box::new(candidate)
                            })
                    })?);
                    Ok(complete.then_some(()))
                })?;
                retained.expect("successful native step retains its candidate")
            }
            BackupSourceStep::Complete => {
                let header = image
                    .header()
                    .ok_or(BackupError::Invalid("missing native image header"))?;
                let (candidate, _memory) = candidate.into_parts();
                return Ok(Some(candidate.finish(header, &self.control)?));
            }
        };
        self.candidate = Some(candidate);
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
        limits: DecodeLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<(Self, bool)> {
        let registry = self
            .storage
            .memory_capture
            .as_ref()
            .expect("private memory destination");
        if let DecodedMemoryImageAddress::Catalog(slot) = address {
            let (catalogs, complete) = self.catalogs.take().ok_or_else(invalid)?.read_step(
                &mut self.durable,
                *slot,
                reader,
                &Destination {
                    registry,
                    mutations: &self.mutations,
                    control,
                    window: limits.window,
                },
            )?;
            self.catalogs = Some(catalogs);
            return Ok((self, complete));
        }
        if let Some(catalogs) = self.catalogs.take() {
            catalogs.finish()?;
        }
        match address {
            DecodedMemoryImageAddress::FieldAnalyzer { .. }
            | DecodedMemoryImageAddress::Resource { .. } => {
                let (resources, complete) = self.resources.read(
                    address,
                    reader,
                    resources,
                    &self.mutations,
                    limits.window,
                    control,
                )?;
                self.resources = resources;
                Ok((self, complete))
            }
            DecodedMemoryImageAddress::Table { unit, .. } => {
                if matches!(unit, DecodedMemoryTableUnit::Metadata(0))
                    && !self.table.as_ref().is_some_and(TableInput::reading_header)
                {
                    if let Some(table) = self.table.take() {
                        let (name, table) = table.finish(&self.storage, control)?;
                        self.tables = self.tables.add_table(name, table)?;
                    }
                    self.table = Some(TableInput::start(address, control)?);
                }
                let (table, complete) = self.table.take().ok_or_else(invalid)?.read(
                    unit,
                    reader,
                    &TableDestination {
                        state: &self.storage,
                        fields: self.resources.fields()?,
                        resources,
                        maximum_value_bytes: limits.value_bytes,
                        window: limits.window,
                        temporary,
                    },
                    control,
                )?;
                self.table = Some(table);
                Ok((self, complete))
            }
            _ => Err(invalid()),
        }
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
