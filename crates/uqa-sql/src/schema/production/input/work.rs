//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{ast::InternalColumnRef, schema::ColumnIdentity, SQLError};
use std::cmp::Ordering;
use uqa_core::memory::{MapKeyInput, OwnedMap, Produced, ProductionControl};

pub(super) struct Work<'a, 'b, W> {
    pub control: ProductionControl<'a>,
    pub input: &'b mut W,
}

impl<W: crate::catalog::CatalogWork + Send> Work<'_, '_, W> {
    pub async fn checkpoint(&mut self) -> Result<(), SQLError> {
        self.control.check()?;
        self.input.checkpoint().await?;
        Ok(self.control.check()?)
    }

    pub async fn text(&mut self, value: &str) -> Result<Produced<String>, SQLError> {
        self.checkpoint().await?;
        crate::catalog::work::Work::text(self.input, value, &self.control).await
    }

    pub async fn boxed_text(&mut self, value: &str) -> Result<Produced<Box<str>>, SQLError> {
        let (value, memory) = self.text(value).await?.into_parts();
        // Native text reserves the complete destination before copying any bytes.
        Ok(self.control.finish(value.into_boxed_str(), memory)?)
    }

    pub async fn identity(
        &mut self,
        qualifier: &str,
        name: &str,
    ) -> Result<Produced<ColumnIdentity>, SQLError> {
        ColumnIdentity::copy_from_work(Some(qualifier), name, &self.control, self.input).await
    }

    pub async fn compare_text(&mut self, left: &str, right: &str) -> Result<Ordering, SQLError> {
        self.checkpoint().await?;
        crate::catalog::work::compare_text(self.input, left, right, &self.control).await
    }

    pub async fn insert<K: Ord, V>(
        &mut self,
        map: &mut OwnedMap<K, Produced<V>>,
        key: Produced<K>,
        value: Produced<V>,
    ) -> Result<bool, SQLError>
    where
        Self: MapKeyInput<K, Error = SQLError>,
    {
        Ok(map
            .insert_produced_key_from(key, value, self)
            .await?
            .is_some())
    }
}

impl<W: crate::catalog::CatalogWork + Send> MapKeyInput<Box<str>> for Work<'_, '_, W> {
    type Error = SQLError;

    async fn checkpoint(&mut self) -> Result<(), SQLError> {
        Work::checkpoint(self).await
    }

    async fn compare(&mut self, left: &Box<str>, right: &Box<str>) -> Result<Ordering, SQLError> {
        self.compare_text(left, right).await
    }
}

impl<W: crate::catalog::CatalogWork + Send> MapKeyInput<ColumnIdentity> for Work<'_, '_, W> {
    type Error = SQLError;

    async fn checkpoint(&mut self) -> Result<(), SQLError> {
        Work::checkpoint(self).await
    }

    async fn compare(
        &mut self,
        left: &ColumnIdentity,
        right: &ColumnIdentity,
    ) -> Result<Ordering, SQLError> {
        crate::catalog::work::ReferenceWork {
            control: &self.control,
            work: self.input,
        }
        .compare(left, right)
        .await
    }
}

impl<W: crate::catalog::CatalogWork + Send> MapKeyInput<InternalColumnRef> for Work<'_, '_, W> {
    type Error = SQLError;

    async fn checkpoint(&mut self) -> Result<(), SQLError> {
        Work::checkpoint(self).await
    }

    async fn compare(
        &mut self,
        left: &InternalColumnRef,
        right: &InternalColumnRef,
    ) -> Result<Ordering, SQLError> {
        self.checkpoint().await?;
        Ok(left.cmp(right))
    }
}
