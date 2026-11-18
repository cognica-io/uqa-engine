//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog predicates share their traversal between inline and retained input.

use crate::SQLError;
use std::{
    future::Future,
    task::{Context, Poll, Waker},
};
use uqa_core::{
    image_io::{AsyncImageInput, ImageIoError},
    memory::{Produced, ProductionControl, ProductionString},
};

pub(crate) trait Work {
    fn checkpoint(&mut self) -> impl Future<Output = Result<(), ImageIoError>> + Send;

    fn text(
        &mut self,
        source: &str,
        control: &ProductionControl<'_>,
    ) -> impl Future<Output = Result<Produced<String>, SQLError>> + Send;
}

impl Work for () {
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

pub(crate) async fn compare_text<W: Work + Send>(
    work: &mut W,
    left: &str,
    right: &str,
    control: &ProductionControl<'_>,
) -> Result<std::cmp::Ordering, SQLError> {
    for (left, right) in left
        .as_bytes()
        .chunks(4096)
        .zip(right.as_bytes().chunks(4096))
    {
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

struct ReferenceWork<'a, 'c, W> {
    control: &'a ProductionControl<'c>,
    work: &'a mut W,
}
impl<W: Work + Send> uqa_core::RelationReferenceWork for ReferenceWork<'_, '_, W> {
    type Error = SQLError;
    async fn checkpoint(&mut self) -> Result<(), SQLError> {
        self.control.check()?;
        self.work.checkpoint().await?;
        self.control.check()?;
        Ok(())
    }
}
