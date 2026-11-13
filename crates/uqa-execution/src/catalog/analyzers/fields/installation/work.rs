//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordinary and retained field installation share order and select their native publication owner.

use super::{corrupt, FieldKey, NativeBinding};
use std::{
    borrow::Borrow,
    fmt,
    future::Future,
    task::{Context, Poll, Waker},
};
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{Budgeted, Produced, ProductionControl},
    CancellationToken, RelationIdentity,
};
use uqa_sql::{ast::ColumnDef, semantics::text_indexes};
use uqa_storage::{
    analyzer_binding::registry::FieldAnalyzerCatalog, FieldAnalyzerBinding, StorageBackendError,
    StorageBackendResult,
};

pub(super) trait Work: Send {
    type Value: Borrow<FieldAnalyzerBinding> + Send;
    fn checkpoint(&mut self) -> impl Future<Output = StorageBackendResult<()>> + Send;
    fn column(
        &mut self,
        columns: &[ColumnDef],
        table: &str,
        field: &str,
    ) -> impl Future<Output = StorageBackendResult<()>> + Send;
    fn name(
        &mut self,
        relation: &RelationIdentity,
    ) -> impl Future<Output = StorageBackendResult<Produced<String>>> + Send;
    fn key(
        &mut self,
        table: &str,
        field: &str,
    ) -> impl Future<Output = StorageBackendResult<Produced<FieldKey>>> + Send;
    fn prepare(
        &mut self,
        catalog: FieldAnalyzerCatalog,
        key: &FieldKey,
        value: Self::Value,
    ) -> impl Future<Output = StorageBackendResult<FieldAnalyzerCatalog>> + Send;
    fn error(&self, message: fmt::Arguments<'_>) -> StorageBackendError;
}

impl Work for () {
    type Value = FieldAnalyzerBinding;
    async fn checkpoint(&mut self) -> StorageBackendResult<()> {
        Ok(())
    }
    async fn column(
        &mut self,
        columns: &[ColumnDef],
        table: &str,
        field: &str,
    ) -> StorageBackendResult<()> {
        text_indexes::validate_analyzer_column(columns, table, field)
            .map_err(|error| self.error(format_args!("{error}")))
    }
    async fn name(
        &mut self,
        relation: &RelationIdentity,
    ) -> StorageBackendResult<Produced<String>> {
        Ok(relation.qualified_name_with_control(&ProductionControl::uncontrolled())?)
    }
    async fn key(&mut self, table: &str, field: &str) -> StorageBackendResult<Produced<FieldKey>> {
        Ok(ProductionControl::uncontrolled().finish((table.to_owned(), field.to_owned()), None)?)
    }
    async fn prepare(
        &mut self,
        mut catalog: FieldAnalyzerCatalog,
        key: &FieldKey,
        value: Self::Value,
    ) -> StorageBackendResult<FieldAnalyzerCatalog> {
        catalog.insert(key, value, false, &CancellationToken::new())?;
        Ok(catalog)
    }
    fn error(&self, message: fmt::Arguments<'_>) -> StorageBackendError {
        corrupt(message.to_string())
    }
}

impl Work for AsyncImageInput<'_> {
    type Value = NativeBinding;
    async fn checkpoint(&mut self) -> StorageBackendResult<()> {
        Ok(AsyncImageInput::checkpoint(self).await?)
    }
    async fn column(
        &mut self,
        columns: &[ColumnDef],
        table: &str,
        field: &str,
    ) -> StorageBackendResult<()> {
        text_indexes::validate_analyzer_column_from(columns, table, field, self)
            .await
            .map_err(|error| StorageBackendError::backend("analyzer column input", error))?
            .map_err(|error| self.error(format_args!("{error}")))
    }
    async fn name(
        &mut self,
        relation: &RelationIdentity,
    ) -> StorageBackendResult<Produced<String>> {
        Ok(relation.qualified_name_from(self).await?.into())
    }
    async fn key(&mut self, table: &str, field: &str) -> StorageBackendResult<Produced<FieldKey>> {
        let (table, mut memory) = self.copy_text(table).await?.into_parts();
        let (field, owned) = self.copy_text(field).await?.into_parts();
        memory.absorb(owned);
        Ok(Budgeted::new((table, field), memory).into())
    }
    async fn prepare(
        &mut self,
        catalog: FieldAnalyzerCatalog,
        _: &FieldKey,
        value: Self::Value,
    ) -> StorageBackendResult<FieldAnalyzerCatalog> {
        catalog
            .insert_restored_from(value.key, value.value, self)
            .await
    }
    fn error(&self, message: fmt::Arguments<'_>) -> StorageBackendError {
        let control = ProductionControl::new(self.memory, self.cancellation, self.cancellation);
        match control.format(message) {
            Ok(message) => corrupt(message.into_parts().0),
            Err(error) => error.into(),
        }
    }
}

pub(super) fn inline<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(result) => result,
        Poll::Pending => unreachable!("ordinary field restoration never suspends"),
    }
}
