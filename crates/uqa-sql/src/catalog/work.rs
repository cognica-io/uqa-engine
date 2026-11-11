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
    image_io::AsyncImageInput,
    memory::{Produced, ProductionControl, ProductionString},
};

pub(crate) trait Work {
    fn checkpoint(&mut self) -> impl Future<Output = Result<(), SQLError>> + Send;

    fn text(
        &mut self,
        source: &str,
        control: &ProductionControl<'_>,
    ) -> impl Future<Output = Result<Produced<String>, SQLError>> + Send;
}

impl Work for () {
    async fn checkpoint(&mut self) -> Result<(), SQLError> {
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
    async fn checkpoint(&mut self) -> Result<(), SQLError> {
        Ok(AsyncImageInput::checkpoint(self).await?)
    }

    async fn text(
        &mut self,
        source: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, SQLError> {
        let mut value = ProductionString::new(*control);
        value.reserve(source.len())?;
        let mut begin = 0;
        while begin < source.len() {
            self.checkpoint().await?;
            control.check()?;
            let mut end = begin.saturating_add(4096).min(source.len());
            while !source.is_char_boundary(end) {
                end -= 1;
            }
            value.push_str(&source[begin..end])?;
            begin = end;
        }
        Ok(value.finish()?)
    }
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
