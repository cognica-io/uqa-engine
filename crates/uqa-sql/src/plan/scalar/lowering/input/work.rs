//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Payload owners share their copy algorithms with independently scheduled plan constructors.

use crate::{ast::ColumnType, construction::Work, SQLError};
use uqa_core::{
    image_io::ImageIoError,
    memory::{Produced, ProductionControl},
    Value, ValueRetentionError,
};

impl uqa_core::ValueCopyWork for Work<'_> {
    async fn checkpoint(&mut self) -> Result<(), ValueRetentionError> {
        Work::checkpoint(self).await;
        Ok(())
    }
}

impl crate::catalog::work::Work for Work<'_> {
    async fn checkpoint(&mut self) -> Result<(), ImageIoError> {
        Work::checkpoint(self).await;
        Ok(())
    }

    async fn text(
        &mut self,
        source: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, SQLError> {
        crate::catalog::work::copy_text(self, source, control).await
    }
}

impl crate::ast::ColumnTypeInputWork for Work<'_> {
    type Error = SQLError;

    async fn checkpoint(&mut self) -> Result<(), Self::Error> {
        Work::checkpoint(self).await;
        Ok(())
    }

    async fn text(
        &mut self,
        source: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, Self::Error> {
        crate::catalog::work::copy_text(self, source, control).await
    }
}

impl crate::plan::scalar::ScalarInputWork for Work<'_> {
    type Error = SQLError;

    async fn checkpoint(&mut self) -> Result<(), Self::Error> {
        Work::checkpoint(self).await;
        Ok(())
    }

    async fn text(
        &mut self,
        source: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, Self::Error> {
        crate::catalog::work::copy_text(self, source, control).await
    }

    async fn ty(
        &mut self,
        ty: &ColumnType,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<ColumnType>, Self::Error> {
        ty.clone_with_input_work(control, self).await
    }

    async fn value(
        &mut self,
        value: &Value,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Value>, Self::Error> {
        Ok(value.clone_from_input(control, self).await?)
    }
}
