//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native analysis constructors share ordinary and retained scheduling.

use std::{
    future::Future,
    task::{Context, Poll, Waker},
};

pub(crate) trait Work<E>: Send {
    fn checkpoint(&mut self) -> impl Future<Output = Result<(), E>> + Send;

    fn records(&mut self, index: usize) -> impl Future<Output = Result<(), E>> + Send {
        async move {
            if index.is_multiple_of(128) {
                self.checkpoint().await?;
            }
            Ok(())
        }
    }
}

impl<E> Work<E> for () {
    async fn checkpoint(&mut self) -> Result<(), E> {
        Ok(())
    }
}

impl<E: From<uqa_core::image_io::ImageIoError>> Work<E>
    for uqa_core::image_io::AsyncImageInput<'_>
{
    async fn checkpoint(&mut self) -> Result<(), E> {
        uqa_core::image_io::AsyncImageInput::checkpoint(self)
            .await
            .map_err(Into::into)
    }
}

pub(crate) fn inline<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => unreachable!("ordinary analysis construction has no suspended work"),
    }
}

/// Copy original text while retaining the position between scalar-aligned fragments.
pub(crate) async fn copy_text<E>(value: &str, work: &mut impl Work<E>) -> Result<String, E> {
    work.checkpoint().await?;
    let mut output = String::with_capacity(value.len());
    let mut position = 0;
    while position < value.len() {
        work.checkpoint().await?;
        let part = crate::json::fragment(value, position);
        output.push_str(part);
        position += part.len();
    }
    Ok(output)
}

pub(crate) async fn own_text<E>(
    value: std::borrow::Cow<'_, str>,
    work: &mut impl Work<E>,
) -> Result<String, E> {
    match value {
        std::borrow::Cow::Owned(value) => Ok(value),
        std::borrow::Cow::Borrowed(value) => copy_text(value, work).await,
    }
}

pub(crate) async fn find_byte<E>(
    value: &str,
    start: usize,
    byte: u8,
    work: &mut impl Work<E>,
) -> Result<Option<usize>, E> {
    for (index, bytes) in value.as_bytes()[start..].chunks(4096).enumerate() {
        work.checkpoint().await?;
        if let Some(offset) = bytes.iter().position(|candidate| *candidate == byte) {
            return Ok(Some(start + index * 4096 + offset));
        }
    }
    Ok(None)
}
