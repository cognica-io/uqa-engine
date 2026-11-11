//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Source adapters own field order, diagnostics and allocation admission.

use serde::de::DeserializeOwned;
use std::{collections::BTreeMap, future::Future};

pub(crate) trait Read: Send {
    type Error: Send;
    fn error(&self, error: serde_json::Error) -> Self::Error;
    fn implicit_defaults(&self) -> bool;
    fn missing(&self, name: &'static str) -> Self::Error;
    #[cfg(any(feature = "nori", feature = "kuromoji"))]
    fn untagged(&self, error: Self::Error, name: &'static str) -> Self::Error;
    #[cfg(any(feature = "nori", feature = "kuromoji"))]
    fn is_text(&mut self) -> impl Future<Output = Result<bool, Self::Error>> + Send;
    fn object(
        &mut self,
        name: &'static str,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
    fn tagged(
        &mut self,
        name: &'static str,
        tag: &'static str,
        variants: &'static [&'static str],
    ) -> impl Future<Output = Result<&'static str, Self::Error>> + Send;
    fn field(
        &mut self,
        names: &'static [&'static str],
        omitted: &'static [bool],
        description: &'static str,
        deny: bool,
        previous: &mut Option<usize>,
    ) -> impl Future<Output = Result<Option<usize>, Self::Error>> + Send;
    fn text(
        &mut self,
        expected: &'static str,
    ) -> impl Future<Output = Result<String, Self::Error>> + Send;
    fn atom<T: DeserializeOwned + Send>(
        &mut self,
    ) -> impl Future<Output = Result<T, Self::Error>> + Send;
    fn null(&mut self) -> impl Future<Output = Result<bool, Self::Error>> + Send;
    fn sequence<T: Item>(&mut self) -> impl Future<Output = Result<Vec<T>, Self::Error>> + Send;
    fn map<T: Item>(
        &mut self,
    ) -> impl Future<Output = Result<BTreeMap<String, T>, Self::Error>> + Send;
}

pub(crate) trait Item: Sized + Send {
    fn read<R: Read>(input: &mut R) -> impl Future<Output = Result<Self, R::Error>> + Send;
}

/// Original images omit absent values instead of encoding JSON null.
pub(crate) struct Omitted<T>(pub Option<T>);
impl<T: Item> Item for Omitted<T> {
    async fn read<R: Read>(input: &mut R) -> Result<Self, R::Error> {
        if input.implicit_defaults() {
            Option::<T>::read(input).await.map(Self)
        } else {
            T::read(input).await.map(|value| Self(Some(value)))
        }
    }
}

impl Item for String {
    async fn read<R: Read>(input: &mut R) -> Result<Self, R::Error> {
        input.text("a string").await
    }
}
impl Item for std::path::PathBuf {
    async fn read<R: Read>(input: &mut R) -> Result<Self, R::Error> {
        input.text("path string").await.map(Into::into)
    }
}
impl<T: Item> Item for Vec<T> {
    async fn read<R: Read>(input: &mut R) -> Result<Self, R::Error> {
        input.sequence().await
    }
}
impl<T: Item> Item for Option<T> {
    async fn read<R: Read>(input: &mut R) -> Result<Self, R::Error> {
        if input.null().await? {
            Ok(None)
        } else {
            T::read(input).await.map(Some)
        }
    }
}
impl<T: Item> Item for BTreeMap<String, T> {
    async fn read<R: Read>(input: &mut R) -> Result<Self, R::Error> {
        input.map().await
    }
}

macro_rules! atom {
    ($($ty:ty),* $(,)?) => {$(
        impl Item for $ty {
            async fn read<R: Read>(input: &mut R) -> Result<Self, R::Error> { input.atom().await }
        }
    )*};
}
atom!(bool, usize, i32);
#[cfg(feature = "nori")]
atom!(crate::nori::DecompoundMode, crate::nori::POSTag);
#[cfg(feature = "kuromoji")]
atom!(
    crate::kuromoji::KuromojiMode,
    crate::kuromoji::CompletionMode
);
