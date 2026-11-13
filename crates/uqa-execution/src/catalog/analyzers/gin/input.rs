//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete physical rows precede retained GIN semantics and per-field publication.

use super::{
    corrupt, owners,
    restoration::{self, Columns, Parameter, Restoration},
    AnalyzerTableDefinition, GinAnalyzerParameter, RetainedGinAnalyzerOwners,
};
use crate::catalog::analyzers::fields::installation::validate_declared_field_from;
use std::{cmp::Ordering, future::Future, ops::Deref};
use uqa_core::{
    image_io::{AsyncImageInput, ImageDecodeWindow, ImageDecoder},
    memory::{Budgeted, MemoryReservation, ProductionControl},
    RelationIdentity, RelationName,
};
use uqa_sql::ast::IndexKey;
use uqa_storage::{
    catalog::input::CatalogInputLimits, read_control::StorageReadControl, CatalogFacade,
    StorageBackendError, StorageBackendResult,
};

/// The selected private table publishes a complete admitted field successor at the original registration boundary.
pub trait GinAnalyzerInputTable: AnalyzerTableDefinition {
    fn register_field_from(
        &self,
        field: &str,
        input: &mut AsyncImageInput<'_>,
    ) -> impl Future<Output = StorageBackendResult<()>> + Send;
}

/// Lookup observes already-restored membership; it does not refresh catalog or data state while a GIN row is being interpreted.
pub trait GinAnalyzerInputTables: Send + Sync {
    type Table: GinAnalyzerInputTable;
    fn table_from(
        &self,
        name: &str,
        input: &mut AsyncImageInput<'_>,
    ) -> impl Future<Output = StorageBackendResult<Option<Self::Table>>> + Send;
}

pub struct GinAnalyzerCatalogInput {
    input: ImageDecoder<RetainedGinAnalyzerOwners, StorageBackendError>,
}

impl GinAnalyzerCatalogInput {
    pub fn new(
        catalog: &dyn CatalogFacade,
        tables: impl GinAnalyzerInputTables + 'static,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        limits.validate()?;
        let mut source = catalog.begin_index_load(limits, control)?;
        let input = ImageDecoder::new(
            control.memory(),
            control.cancellation(),
            |mut reader| async move {
                let mut input = reader.input();
                let rows = loop {
                    input.checkpoint().await?;
                    if let Some(rows) = source.advance(1)? {
                        break rows;
                    }
                };
                drop(source);
                let mut work = Native {
                    tables,
                    owners: RetainedGinAnalyzerOwners::new(),
                    input: &mut input,
                };
                let (rows, _row_memory) = rows.into_parts();
                for row in rows {
                    restoration::restore_row(&row, &mut work).await?;
                }
                Ok(work.owners)
            },
        )?;
        Ok(Self { input })
    }

    pub fn advance(
        &mut self,
        steps: usize,
    ) -> StorageBackendResult<Option<RetainedGinAnalyzerOwners>> {
        self.input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
    }
}

struct Name(Budgeted<String>);
impl Deref for Name {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

struct NativeColumns {
    items: std::vec::IntoIter<Budgeted<String>>,
    _memory: MemoryReservation,
}
impl Columns for NativeColumns {
    type Column = Name;
    fn next(&mut self) -> Option<Name> {
        self.items.next().map(Name)
    }
}
impl Parameter for Option<GinAnalyzerParameter> {
    fn name(&self) -> Option<&str> {
        self.as_ref().map(GinAnalyzerParameter::name)
    }
}

struct Native<'a, 'c, T> {
    tables: T,
    owners: RetainedGinAnalyzerOwners,
    input: &'a mut AsyncImageInput<'c>,
}

impl<T: GinAnalyzerInputTables> Restoration for Native<'_, '_, T> {
    type Name = Name;
    type Table = T::Table;
    type Columns = NativeColumns;
    type Parameter = Option<GinAnalyzerParameter>;

    async fn checkpoint(&mut self) -> StorageBackendResult<()> {
        Ok(self.input.checkpoint().await?)
    }
    async fn name(&mut self, text: &str) -> StorageBackendResult<Name> {
        let mut reference = RelationIdentity::try_borrowed_reference(text).map_err(|error| {
            let control = ProductionControl::new(
                self.input.memory,
                self.input.cancellation,
                self.input.cancellation,
            );
            match control.format(format_args!("{error}")) {
                Ok(message) => corrupt(message.into_parts().0),
                Err(error) => error.into(),
            }
        })?;
        if reference.schema.is_none() {
            reference.schema = Some(RelationName::decoded("public"));
        }
        Ok(Name(reference.render_from(self.input).await?))
    }
    async fn table(&mut self, name: &str) -> StorageBackendResult<Option<T::Table>> {
        self.tables.table_from(name, self.input).await
    }
    async fn columns(&mut self, text: &str) -> StorageBackendResult<NativeColumns> {
        let columns = IndexKey::read_stored_columns_from(text, self.input)
            .await
            .map_err(StorageBackendError::from)?;
        let (items, memory) = columns.into_parts();
        Ok(NativeColumns {
            items: items.into_iter(),
            _memory: memory,
        })
    }
    async fn parameter(&mut self, text: &str) -> StorageBackendResult<Self::Parameter> {
        GinAnalyzerParameter::read_from(text, self.input)
            .await
            .map_err(StorageBackendError::from)
    }
    async fn equal(&mut self, left: &str, right: &str) -> StorageBackendResult<bool> {
        Ok(owners::compare(left, right, self.input).await? == Ordering::Equal)
    }
    async fn register(&mut self, table: &T::Table, field: &str) -> StorageBackendResult<()> {
        table.register_field_from(field, self.input).await
    }
    async fn validate(
        &mut self,
        table: &T::Table,
        name: &str,
        field: &str,
    ) -> StorageBackendResult<()> {
        validate_declared_field_from(table, name, field, self.input).await
    }
    async fn owner(
        &mut self,
        table: &str,
        field: Name,
        analyzer: &str,
    ) -> StorageBackendResult<()> {
        let (table, mut memory) = self.input.copy_text(table).await?.into_parts();
        let (field, field_memory) = field.0.into_parts();
        memory.absorb(field_memory);
        let key = Budgeted::new((table, field), memory);
        let name = self.input.copy_text(analyzer).await?;
        if let Some(previous) = self.owners.insert_from(key, name, self.input).await? {
            if owners::compare(&previous, analyzer, self.input).await? != Ordering::Equal {
                return Err(corrupt("GIN definitions have competing analyzer owners"));
            }
        }
        Ok(())
    }
}
