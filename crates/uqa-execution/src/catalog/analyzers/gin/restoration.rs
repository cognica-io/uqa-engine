//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordinary and retained GIN restoration share each original semantic boundary.

use super::{corrupt, AnalyzerTableDefinition};
use std::{future::Future, ops::Deref};
use uqa_storage::{CatalogIndexRow, StorageBackendResult};

pub(super) trait Columns: Send {
    type Column: Deref<Target = str> + Send;
    fn next(&mut self) -> Option<Self::Column>;
}

pub(super) trait Parameter: Send {
    fn name(&self) -> Option<&str>;
}

pub(super) trait Restoration: Send {
    type Name: Deref<Target = str> + Send;
    type Table: AnalyzerTableDefinition;
    type Columns: Columns;
    type Parameter: Parameter;

    fn checkpoint(&mut self) -> impl Future<Output = StorageBackendResult<()>> + Send;
    fn name(&mut self, text: &str)
        -> impl Future<Output = StorageBackendResult<Self::Name>> + Send;
    fn table(
        &mut self,
        name: &str,
    ) -> impl Future<Output = StorageBackendResult<Option<Self::Table>>> + Send;
    fn columns(
        &mut self,
        text: &str,
    ) -> impl Future<Output = StorageBackendResult<Self::Columns>> + Send;
    fn parameter(
        &mut self,
        text: &str,
    ) -> impl Future<Output = StorageBackendResult<Self::Parameter>> + Send;
    fn equal(
        &mut self,
        left: &str,
        right: &str,
    ) -> impl Future<Output = StorageBackendResult<bool>> + Send;
    fn register(
        &mut self,
        table: &Self::Table,
        field: &str,
    ) -> impl Future<Output = StorageBackendResult<()>> + Send;
    fn validate(
        &mut self,
        table: &Self::Table,
        name: &str,
        field: &str,
    ) -> impl Future<Output = StorageBackendResult<()>> + Send;
    fn owner(
        &mut self,
        table: &str,
        field: <Self::Columns as Columns>::Column,
        analyzer: &str,
    ) -> impl Future<Output = StorageBackendResult<()>> + Send;
}

pub(super) async fn restore_row(
    row: &CatalogIndexRow,
    work: &mut impl Restoration,
) -> StorageBackendResult<()> {
    work.checkpoint().await?;
    if !row.index_type.eq_ignore_ascii_case("gin") {
        return Ok(());
    }
    let name = work.name(&row.table_name).await?;
    let table = work
        .table(&name)
        .await?
        .ok_or_else(|| corrupt("GIN analyzer owner references a missing table"))?;
    let mut columns = work.columns(&row.columns_json).await?;
    let parameter = work.parameter(&row.parameters_json).await?;
    while let Some(field) = columns.next() {
        work.checkpoint().await?;
        let fields = table.fields();
        let mut registered = false;
        for current in fields.iter() {
            if work.equal(current, &field).await? {
                registered = true;
                break;
            }
        }
        if !registered {
            work.register(&table, &field).await?;
        }
        if table.columns_declared() {
            work.validate(&table, &name, &field).await?;
        }
        if let Some(analyzer) = parameter.name() {
            work.owner(&name, field, analyzer).await?;
        }
    }
    Ok(())
}
