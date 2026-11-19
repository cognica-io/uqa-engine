//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain native routine parameter fields with the existing SQL syntax projections and constructor driver.

use super::{Copying, Result, SyntaxCopy};
use crate::{ast, construction, plan::copy::CopyPurpose};
use std::future::Future;
use uqa_core::memory::{MemoryError, Produced, ProductionControl};

pub(in crate::plan::copy) struct Input<'s> {
    copying: Copying<'s>,
    pub(super) work: construction::Work<'s>,
}

pub(in crate::plan::copy) trait Field: SyntaxCopy + Send + Sync {
    fn copy_from<'s>(&'s self, input: &mut Input<'s>) -> impl Future<Output = Result<Self>> + Send {
        std::future::ready(self.copy_syntax(&mut input.copying))
    }
}

impl<'s> Input<'s> {
    pub(in crate::plan::copy) fn new(
        control: ProductionControl<'s>,
        work: construction::Work<'s>,
    ) -> Self {
        Self {
            copying: Copying::capture(control, CopyPurpose::Stored),
            work,
        }
    }
    pub(super) fn inline<T: SyntaxCopy>(&mut self, value: &T) -> Result<T> {
        value.copy_syntax(&mut self.copying)
    }

    pub(super) fn control(&self) -> ProductionControl<'s> {
        self.copying.control
    }

    pub(super) async fn field<T: Field>(&mut self, value: &'s T) -> Result<T> {
        value.copy_from(self).await
    }

    pub(in crate::plan::copy) fn finish<T>(self, value: T) -> Result<(Produced<T>, usize)> {
        let children = self.copying.retained_children;
        Ok((self.copying.finish(value)?, children))
    }

    pub(super) fn take_child<T>(&mut self, (value, children): (Produced<T>, usize)) -> Result<T> {
        self.copying.retained_children = self
            .copying
            .retained_children
            .checked_add(children)
            .ok_or(MemoryError::SizeOverflow)?;
        Ok(self.copying.take(value))
    }
}

macro_rules! inline_fields {
    ($($ty:ty),* $(,)?) => { $(impl Field for $ty {})* };
}

// A query or window is rejected by routine parameter analysis. Preserve its existing stored projection and subsequent diagnostic when such syntax is supplied privately.
inline_fields!(
    ast::BinaryOp,
    ast::FunctionOrderSyntax,
    ast::FunctionParamMode,
    ast::InternalColumnRef,
    ast::NullsOrder,
    ast::WindowCallModifiers,
    ast::WindowSpec,
    ast::SelectStmt,
    ast::CompositeRowLayout,
    ast::RetainedInputLiteral,
    bool,
    usize,
    u32,
    i16
);

macro_rules! payload {
    ($ty:ty, $method:ident) => {
        impl Field for $ty {
            async fn copy_from<'s>(&'s self, input: &mut Input<'s>) -> Result<Self> {
                let value = self.$method(&input.control(), &mut input.work).await?;
                Ok(input.copying.take(value))
            }
        }
    };
}
payload!(uqa_core::Value, clone_from_input);
payload!(ast::FunctionBinding, clone_with_input_work);
payload!(crate::ColumnType, clone_with_input_work);
payload!(
    crate::expr::composites::CompositeTypeDescriptor,
    clone_with_input_work
);

impl Field for String {
    async fn copy_from<'s>(&'s self, input: &mut Input<'s>) -> Result<Self> {
        let value =
            crate::catalog::work::copy_text(&mut input.work, self, &input.copying.control).await?;
        Ok(input.copying.take(value))
    }
}
impl Field for ast::ReferenceName {
    async fn copy_from<'s>(&'s self, input: &mut Input<'s>) -> Result<Self> {
        let text =
            crate::catalog::work::copy_text(&mut input.work, self.as_str(), &input.copying.control)
                .await?;
        Ok(Self::from(input.copying.take(text)).with_position(self.position()))
    }
}
impl<T: Field> Field for Option<T> {
    async fn copy_from<'s>(&'s self, input: &mut Input<'s>) -> Result<Self> {
        match self {
            Some(value) => Ok(Some(input.field(value).await?)),
            None => Ok(None),
        }
    }
}
impl<T: Field> Field for Box<T> {
    async fn copy_from<'s>(&'s self, input: &mut Input<'s>) -> Result<Self> {
        input.copying.admit_box::<T>()?;
        let child = input.field(self.as_ref()).await?;
        input.copying.finish_box(child, std::convert::identity)
    }
}
impl<T: Field> Field for Vec<T> {
    async fn copy_from<'s>(&'s self, input: &mut Input<'s>) -> Result<Self> {
        let mut values = input.copying.reserve_slots(self.len())?;
        for source in self {
            input.work.checkpoint().await;
            values.push(input.field(source).await?);
        }
        Ok(values)
    }
}
impl<A: Field, B: Field> Field for (A, B) {
    async fn copy_from<'s>(&'s self, input: &mut Input<'s>) -> Result<Self> {
        Ok((input.field(&self.0).await?, input.field(&self.1).await?))
    }
}
