//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Payload constructors share admission and field order between inline and suspended work.

mod composites;

use super::Lowering;
use crate::{ast::ColumnType, schema::retention::CatalogRetentionError, SQLError};
use std::future::Future;
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{Produced, ProductionControl},
    Value, ValueRetentionError,
};

pub(in crate::plan) trait Work: Send {
    type Error: From<ValueRetentionError>;

    fn checkpoint(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send;
    fn text(
        &mut self,
        text: &str,
        control: &ProductionControl<'_>,
    ) -> impl Future<Output = Result<Produced<String>, Self::Error>> + Send;
    fn ty(
        &mut self,
        ty: &ColumnType,
        control: &ProductionControl<'_>,
    ) -> impl Future<Output = Result<Produced<ColumnType>, Self::Error>> + Send;
    fn value(
        &mut self,
        value: &Value,
        control: &ProductionControl<'_>,
    ) -> impl Future<Output = Result<Produced<Value>, Self::Error>> + Send;
}

impl Work for () {
    type Error = CatalogRetentionError;

    async fn checkpoint(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn text(
        &mut self,
        text: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, Self::Error> {
        Ok(control.copy_text(text)?)
    }

    async fn ty(
        &mut self,
        ty: &ColumnType,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<ColumnType>, Self::Error> {
        Ok(ty.clone_with_control(control)?)
    }

    async fn value(
        &mut self,
        value: &Value,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Value>, Self::Error> {
        Ok(control.copy_value(value)?)
    }
}

impl Work for AsyncImageInput<'_> {
    type Error = SQLError;

    async fn checkpoint(&mut self) -> Result<(), Self::Error> {
        Ok(AsyncImageInput::checkpoint(self).await?)
    }

    async fn text(
        &mut self,
        text: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, Self::Error> {
        crate::catalog::work::Work::text(self, text, control).await
    }

    async fn ty(
        &mut self,
        ty: &ColumnType,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<ColumnType>, Self::Error> {
        ty.clone_from_input(control, self).await
    }

    async fn value(
        &mut self,
        value: &Value,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Value>, Self::Error> {
        Ok(value.clone_from_input(control, self).await?)
    }
}

impl Lowering<'_> {
    pub(in crate::plan::scalar) fn production(&self) -> ProductionControl<'_> {
        self.control
            .as_ref()
            .map_or_else(ProductionControl::uncontrolled, |owner| owner.production)
    }

    pub(in crate::plan::scalar) fn take_input<T>(&mut self, value: Produced<T>) -> T {
        let (value, memory) = value.into_parts();
        if let Some(memory) = memory {
            self.control
                .as_mut()
                .expect("controlled scalar payload")
                .memory
                .absorb(memory);
        }
        value
    }

    pub(in crate::plan::scalar) async fn checkpoint<W: Work>(
        &self,
        work: &mut W,
    ) -> Result<(), W::Error> {
        self.production().check()?;
        work.checkpoint().await?;
        self.production().check()?;
        Ok(())
    }

    pub(in crate::plan::scalar) async fn text_from<W: Work>(
        &mut self,
        text: &str,
        work: &mut W,
    ) -> Result<String, W::Error> {
        self.checkpoint(work).await?;
        let copied = work.text(text, &self.production()).await?;
        Ok(self.take_input(copied))
    }

    pub(in crate::plan::scalar) async fn optional_text_from<W: Work>(
        &mut self,
        text: Option<&String>,
        work: &mut W,
    ) -> Result<Option<String>, W::Error> {
        match text {
            Some(text) => Ok(Some(self.text_from(text, work).await?)),
            None => Ok(None),
        }
    }

    pub(in crate::plan::scalar) async fn reference_from<W: Work>(
        &mut self,
        name: &crate::ast::ReferenceName,
        work: &mut W,
    ) -> Result<crate::ast::ReferenceName, W::Error> {
        Ok(
            crate::ast::ReferenceName::from(self.text_from(name, work).await?)
                .with_position(name.position()),
        )
    }

    pub(in crate::plan::scalar) async fn value_from<W: Work>(
        &mut self,
        value: &Value,
        work: &mut W,
    ) -> Result<Value, W::Error> {
        self.checkpoint(work).await?;
        let copied = work.value(value, &self.production()).await?;
        Ok(self.take_input(copied))
    }

    pub(in crate::plan::scalar) async fn type_from<W: Work>(
        &mut self,
        ty: Option<&ColumnType>,
        work: &mut W,
    ) -> Result<Option<ColumnType>, W::Error> {
        match ty {
            Some(ty) => {
                self.checkpoint(work).await?;
                let copied = work.ty(ty, &self.production()).await?;
                Ok(Some(self.take_input(copied)))
            }
            None => Ok(None),
        }
    }
}

// Keep child futures in their owning constructor rather than allocating callback futures.
macro_rules! map_from {
    ($owner:expr, $items:expr, $work:expr, |$item:pat_param| $produce:expr) => {{
        $owner.checkpoint($work).await?;
        let control = $owner
            .control
            .as_ref()
            .map_or_else(uqa_core::memory::ProductionControl::uncontrolled, |owner| {
                owner.production
            });
        let items = $items;
        let mut output = uqa_core::memory::ProductionVec::new(control);
        output.reserve(items.len())?;
        for $item in items {
            $owner.checkpoint($work).await?;
            let value = $produce;
            output.push_produced(control.finish(value, control.empty_reservation())?)?;
        }
        $owner.take_input(output.finish()?)
    }};
}

macro_rules! boxed_from {
    ($owner:expr, $work:expr, $ty:ty, $produce:expr) => {{
        $owner.checkpoint($work).await?;
        let memory = $owner.production().reserve(size_of::<$ty>())?;
        let value: $ty = $produce;
        let value = $owner.production().finish(Box::new(value), memory)?;
        $owner.take_input(value)
    }};
}
pub(in crate::plan::scalar) use {boxed_from, map_from};
