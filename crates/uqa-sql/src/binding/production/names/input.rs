//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lexical names use the existing ordered map's retained comparisons and insertion owner.

use super::{BindingMap, BindingName, ProductionControl, SQLError};
use crate::catalog::work::{compare_text, Work};
use std::cmp::Ordering;
use uqa_core::memory::MapKeyInput;

struct Comparison<'a, 'c, W> {
    work: &'a mut W,
    control: &'a ProductionControl<'c>,
}

impl<W: Work + Send> MapKeyInput<str, BindingName> for Comparison<'_, '_, W> {
    type Error = SQLError;
    async fn checkpoint(&mut self) -> Result<(), SQLError> {
        self.control.check()?;
        self.work.checkpoint().await?;
        self.control.check()?;
        Ok(())
    }
    async fn compare(&mut self, query: &str, stored: &BindingName) -> Result<Ordering, SQLError> {
        compare_text(self.work, query, &stored.0, self.control).await
    }
}

impl<W: Work + Send> MapKeyInput<BindingName> for Comparison<'_, '_, W> {
    type Error = SQLError;
    async fn checkpoint(&mut self) -> Result<(), SQLError> {
        <Self as MapKeyInput<str, BindingName>>::checkpoint(self).await
    }
    async fn compare(
        &mut self,
        query: &BindingName,
        stored: &BindingName,
    ) -> Result<Ordering, SQLError> {
        compare_text(self.work, &query.0, &stored.0, self.control).await
    }
}

impl<V> BindingMap<V> {
    pub(crate) async fn get_from<W: Work + Send>(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<Option<&V>, SQLError> {
        self.entries
            .get_from(name, &mut Comparison { work, control })
            .await
    }

    pub(crate) async fn insert_from<W: Work + Send>(
        &mut self,
        name: &str,
        value: V,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<Option<V>, SQLError> {
        if let Some(previous) = self
            .entries
            .get_mut_from(name, &mut Comparison { work, control })
            .await?
        {
            return Ok(Some(std::mem::replace(previous, value)));
        }
        let name = BindingName(work.text(name, control).await?);
        let Some(budget) = control.budget() else {
            return Ok(self.entries.insert(name, value));
        };
        let key = uqa_core::memory::Budgeted::new(name, budget.reserve(0)?);
        self.entries
            .insert_admitted_key_from(key, value, &mut Comparison { work, control })
            .await
    }

    pub(crate) async fn retire_from<W: Work + Send>(
        self,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<(), SQLError> {
        let mut entries = self.entries.into_iter();
        while entries.len() != 0 {
            control.check()?;
            work.checkpoint().await?;
            drop(entries.next());
        }
        Ok(())
    }
}
