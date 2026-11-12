//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cooperative native vectors retain completed elements while the next field is incomplete.

use super::Fields;
use crate::SQLError;
use std::future::Future;
use uqa_core::memory::{Budgeted, BudgetedVec};

pub(super) trait ImageValue: Sized + Send {
    fn read(
        input: &mut Fields<'_>,
    ) -> impl Future<Output = Result<Budgeted<Self>, SQLError>> + Send;
}

impl Fields<'_> {
    pub(super) async fn image_values<T: ImageValue>(
        &mut self,
        count: usize,
    ) -> Result<Budgeted<Vec<T>>, SQLError> {
        let mut memory = self.binary.memory.empty_reservation();
        let mut values = BudgetedVec::new(self.binary.memory);
        values.reserve(count)?;
        for _ in 0..count {
            self.binary.checkpoint().await?;
            let (value, reservation) = T::read(self).await?.into_parts();
            memory.absorb(reservation);
            values.push(value)?;
        }
        let (values, reservation) = values.into_parts();
        memory.absorb(reservation);
        Ok(Budgeted::new(values, memory))
    }
}

impl ImageValue for usize {
    async fn read(input: &mut Fields<'_>) -> Result<Budgeted<Self>, SQLError> {
        Ok(Budgeted::new(
            input.binary.count().await?,
            input.binary.memory.empty_reservation(),
        ))
    }
}
impl ImageValue for i16 {
    async fn read(input: &mut Fields<'_>) -> Result<Budgeted<Self>, SQLError> {
        Ok(Budgeted::new(
            i16::from_le_bytes(input.binary.array().await?),
            input.binary.memory.empty_reservation(),
        ))
    }
}
impl ImageValue for crate::ast::ColumnType {
    async fn read(input: &mut Fields<'_>) -> Result<Budgeted<Self>, SQLError> {
        input.column_type().await
    }
}
impl ImageValue for String {
    async fn read(input: &mut Fields<'_>) -> Result<Budgeted<Self>, SQLError> {
        Ok(input.binary.text().await?)
    }
}
impl ImageValue for Option<String> {
    async fn read(input: &mut Fields<'_>) -> Result<Budgeted<Self>, SQLError> {
        input.optional_text().await
    }
}
