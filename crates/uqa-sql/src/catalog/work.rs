//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog predicates share their traversal between inline and retained input.

use crate::SQLError;
use std::{
    borrow::Borrow,
    cmp::Ordering,
    future::Future,
    task::{Context, Poll, Waker},
};
use uqa_core::{
    image_io::{AsyncImageInput, ImageIoError},
    memory::{MapKeyInput, Produced, ProductionControl, ProductionString},
    RelationIdentity,
};

/// Run nested input on the caller's stack for inline work and in an admitted frame when it can suspend.
macro_rules! nested {
    ($suspend:expr, $control:expr, $future:expr) => {{
        if $suspend {
            let control = $control.capture();
            $crate::construction::retain_with(control.control(), || $future)?.await
        } else {
            $crate::catalog::work::inline($future)
        }
    }};
}
pub(crate) use nested;

/// Schedule borrowed catalog validation and text construction through the invoking operation.
pub trait Work {
    /// An inline caller can finish a selected field on its stack. Other callers retain that field across checkpoints.
    const MAY_SUSPEND: bool = true;

    fn checkpoint(&mut self) -> impl Future<Output = Result<(), ImageIoError>> + Send;

    fn text(
        &mut self,
        source: &str,
        control: &ProductionControl<'_>,
    ) -> impl Future<Output = Result<Produced<String>, SQLError>> + Send;
}

impl Work for () {
    const MAY_SUSPEND: bool = false;

    async fn checkpoint(&mut self) -> Result<(), ImageIoError> {
        Ok(())
    }

    async fn text(
        &mut self,
        source: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, SQLError> {
        Ok(control.copy_text(source)?)
    }
}

impl Work for AsyncImageInput<'_> {
    async fn checkpoint(&mut self) -> Result<(), ImageIoError> {
        AsyncImageInput::checkpoint(self).await
    }

    async fn text(
        &mut self,
        source: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, SQLError> {
        copy_text(self, source, control).await
    }
}

pub(crate) async fn copy_text<W: Work + Send>(
    work: &mut W,
    source: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, SQLError> {
    text_parts(work, &[source], control).await
}

pub(crate) async fn text_parts<W: Work + Send>(
    work: &mut W,
    parts: &[&str],
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, SQLError> {
    let mut value = ProductionString::new(*control);
    let length = parts
        .iter()
        .try_fold(0usize, |size, part| size.checked_add(part.len()))
        .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
    value.reserve(length)?;
    for source in parts {
        let mut begin = 0;
        while begin < source.len() {
            work.checkpoint().await?;
            control.check()?;
            begin += value.push_str_chunk(&source[begin..])?;
        }
    }
    Ok(value.finish()?)
}

pub(crate) fn text_chunks<'a, 'b>(
    left: &'a str,
    right: &'b str,
) -> impl Iterator<Item = (&'a [u8], &'b [u8])> {
    left.as_bytes()
        .chunks(4096)
        .zip(right.as_bytes().chunks(4096))
}

pub(crate) async fn compare_text<W: Work>(
    work: &mut W,
    left: &str,
    right: &str,
    control: &ProductionControl<'_>,
) -> Result<std::cmp::Ordering, SQLError> {
    for (left, right) in text_chunks(left, right) {
        control.check()?;
        work.checkpoint().await?;
        control.check()?;
        let order = left.cmp(right);
        if order.is_ne() {
            return Ok(order);
        }
    }
    Ok(left.len().cmp(&right.len()))
}

/// Retire one originally owned entry per work checkpoint. An interrupted continuation still owns all pending entries.
pub(crate) async fn retire_entries(
    mut entries: impl ExactSizeIterator,
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> Result<(), SQLError> {
    while entries.len() != 0 {
        control.check()?;
        work.checkpoint().await?;
        control.check()?;
        drop(entries.next());
    }
    Ok(())
}

pub(crate) fn inline<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => unreachable!("inline catalog work never suspends"),
    }
}

pub(crate) async fn reference<'a, W: Work + Send>(
    value: &'a str,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Option<uqa_core::RelationReference<'a>>, SQLError> {
    match uqa_core::RelationIdentity::try_borrowed_reference_from(
        value,
        &mut ReferenceWork { control, work },
    )
    .await
    {
        Ok(value) => Ok(Some(value)),
        Err(uqa_core::RelationReferenceInputError::Reference(_)) => Ok(None),
        Err(uqa_core::RelationReferenceInputError::Input(error)) => Err(error),
    }
}

pub(crate) async fn decoded_name<W: Work + Send>(
    name: uqa_core::RelationName<'_>,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Produced<String>, SQLError> {
    name.decode_from(control, &mut ReferenceWork { control, work })
        .await
}

pub(crate) struct ReferenceWork<'a, 'c, W> {
    pub(crate) control: &'a ProductionControl<'c>,
    pub(crate) work: &'a mut W,
}

impl<W: Work + Send> ReferenceWork<'_, '_, W> {
    pub(crate) async fn checkpoint(&mut self) -> Result<(), SQLError> {
        self.control.check()?;
        self.work.checkpoint().await?;
        self.control.check()?;
        Ok(())
    }
}

impl<W: Work + Send> uqa_core::ValueInputWork for ReferenceWork<'_, '_, W> {
    type Error = SQLError;
    async fn checkpoint(&mut self) -> Result<(), Self::Error> {
        ReferenceWork::checkpoint(self).await
    }
}

impl<W: Work + Send> uqa_core::RelationReferenceWork for ReferenceWork<'_, '_, W> {
    type Error = SQLError;

    async fn checkpoint(&mut self) -> Result<(), SQLError> {
        ReferenceWork::checkpoint(self).await
    }
}

impl<W: Work + Send> crate::ast::ColumnTypeInputWork for ReferenceWork<'_, '_, W> {
    type Error = SQLError;

    async fn checkpoint(&mut self) -> Result<(), SQLError> {
        Ok(self.work.checkpoint().await?)
    }

    async fn text(
        &mut self,
        source: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, SQLError> {
        self.work.text(source, control).await
    }
}

impl<W: Work + Send> uqa_core::ValueCopyWork for ReferenceWork<'_, '_, W> {
    async fn checkpoint(&mut self) -> Result<(), uqa_core::ValueRetentionError> {
        Ok(self.work.checkpoint().await?)
    }
}

impl<W: Work + Send> crate::plan::ScalarInputWork for ReferenceWork<'_, '_, W> {
    type Error = SQLError;

    async fn checkpoint(&mut self) -> Result<(), SQLError> {
        ReferenceWork::checkpoint(self).await
    }

    async fn text(
        &mut self,
        value: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, SQLError> {
        self.work.text(value, control).await
    }

    async fn ty(
        &mut self,
        value: &crate::ColumnType,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<crate::ColumnType>, SQLError> {
        value.clone_with_input_work(control, self).await
    }

    async fn value(
        &mut self,
        value: &uqa_core::Value,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<uqa_core::Value>, SQLError> {
        Ok(value.clone_from_input(control, self).await?)
    }
}

impl<Q: Borrow<RelationIdentity> + Sync, W: Work + Send> MapKeyInput<Q, &RelationIdentity>
    for ReferenceWork<'_, '_, W>
{
    type Error = SQLError;

    async fn checkpoint(&mut self) -> Result<(), SQLError> {
        ReferenceWork::checkpoint(self).await
    }

    async fn compare(
        &mut self,
        query: &Q,
        stored: &&RelationIdentity,
    ) -> Result<Ordering, SQLError> {
        let query = query.borrow();
        let schema = compare_text(self.work, &query.schema, &stored.schema, self.control).await?;
        if schema.is_eq() {
            compare_text(self.work, &query.name, &stored.name, self.control).await
        } else {
            Ok(schema)
        }
    }
}

impl<Q: Borrow<str> + Sync + ?Sized, W: Work + Send> MapKeyInput<Q, &str>
    for ReferenceWork<'_, '_, W>
{
    type Error = SQLError;

    async fn checkpoint(&mut self) -> Result<(), SQLError> {
        Self::checkpoint(self).await
    }

    async fn compare(&mut self, query: &Q, stored: &&str) -> Result<Ordering, SQLError> {
        compare_text(self.work, query.borrow(), stored, self.control).await
    }
}

impl<'q, 's, W: Work + Send> MapKeyInput<([u8; 16], &'q str), ([u8; 16], &'s str)>
    for ReferenceWork<'_, '_, W>
{
    type Error = SQLError;

    async fn checkpoint(&mut self) -> Result<(), SQLError> {
        Self::checkpoint(self).await
    }

    async fn compare(
        &mut self,
        query: &([u8; 16], &'q str),
        stored: &([u8; 16], &'s str),
    ) -> Result<Ordering, SQLError> {
        let table = query.0.cmp(&stored.0);
        if table.is_eq() {
            compare_text(self.work, query.1, stored.1, self.control).await
        } else {
            Ok(table)
        }
    }
}
