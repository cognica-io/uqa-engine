//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preparation shares retained work with existing thread-local callback APIs.

use std::future::{ready, Future};

pub(crate) trait Progress<E> {
    fn checkpoint(&mut self) -> impl Future<Output = Result<(), E>>;
}

#[cfg(feature = "kuromoji")]
pub(crate) struct Retained<'a, W>(pub &'a mut W);

#[cfg(feature = "kuromoji")]
impl<E, W: super::Work<E>> Progress<E> for Retained<'_, W> {
    fn checkpoint(&mut self) -> impl Future<Output = Result<(), E>> {
        super::Work::checkpoint(self.0)
    }
}

pub(crate) struct Callback<'a, E>(pub &'a mut dyn FnMut() -> Result<(), E>);

impl<E> Progress<E> for Callback<'_, E> {
    fn checkpoint(&mut self) -> impl Future<Output = Result<(), E>> {
        ready((self.0)())
    }
}

#[cfg(feature = "kuromoji")]
pub(crate) struct Counter<'a, P> {
    progress: &'a mut P,
    count: usize,
}

#[cfg(feature = "kuromoji")]
impl<'a, P> Counter<'a, P> {
    pub async fn new<E>(progress: &'a mut P) -> Result<Self, E>
    where
        P: Progress<E>,
    {
        progress.checkpoint().await?;
        Ok(Self { progress, count: 0 })
    }
}

#[cfg(feature = "kuromoji")]
impl<E, P: Progress<E>> Progress<E> for Counter<'_, P> {
    async fn checkpoint(&mut self) -> Result<(), E> {
        self.count = (self.count + 1) % 1024;
        if self.count == 0 {
            self.progress.checkpoint().await?;
        }
        Ok(())
    }
}
