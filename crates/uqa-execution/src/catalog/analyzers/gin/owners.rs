//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! GIN ownership keeps admitted keys and names through the complete field-binding boundary.

use crate::catalog::analyzers::fields::{FieldKey, GinAnalyzerOwners};
use std::{cmp::Ordering, future::Future, sync::Arc};
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{Budgeted, MapKeyInput, OwnedMap},
};
use uqa_storage::{StorageBackendError, StorageBackendResult};

/// Complete earlier GIN ownership is borrowed during field cross-validation. A native owner can move directly into the field input; ordinary restoration keeps its existing shared map.
pub trait GinAnalyzerLookup: Send + Sync {
    fn find_from<'a>(
        &'a self,
        key: &'a FieldKey,
        input: &mut AsyncImageInput<'_>,
    ) -> impl Future<Output = StorageBackendResult<Option<&'a str>>> + Send;
}

impl GinAnalyzerLookup for Arc<GinAnalyzerOwners> {
    async fn find_from<'a>(
        &'a self,
        key: &'a FieldKey,
        input: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<Option<&'a str>> {
        input.checkpoint().await?;
        Ok(self.get(key).map(String::as_str))
    }
}

#[derive(Default)]
pub struct RetainedGinAnalyzerOwners {
    owners: OwnedMap<FieldKey, Budgeted<String>>,
}

impl RetainedGinAnalyzerOwners {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adopt the canonical key and selected analyzer allocation. An exact replacement retires its redundant input key and preserves the original key/node lease. The GIN restoration owner checks the returned preceding name at its original per-column conflict boundary.
    pub async fn insert_from(
        &mut self,
        key: Budgeted<FieldKey>,
        analyzer: Budgeted<String>,
        input: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<Option<Budgeted<String>>> {
        self.owners
            .insert_admitted_key_from(key, analyzer, &mut Comparison(input))
            .await
    }
}

impl GinAnalyzerLookup for RetainedGinAnalyzerOwners {
    async fn find_from<'a>(
        &'a self,
        key: &'a FieldKey,
        input: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<Option<&'a str>> {
        Ok(self
            .owners
            .get_from(key, &mut Comparison(input))
            .await?
            .map(|value| value.as_str()))
    }
}

struct Comparison<'a, 'c>(&'a mut AsyncImageInput<'c>);

impl MapKeyInput<FieldKey> for Comparison<'_, '_> {
    type Error = StorageBackendError;

    async fn checkpoint(&mut self) -> StorageBackendResult<()> {
        Ok(self.0.checkpoint().await?)
    }

    async fn compare(
        &mut self,
        query: &FieldKey,
        stored: &FieldKey,
    ) -> StorageBackendResult<Ordering> {
        let table = compare(&query.0, &stored.0, self.0).await?;
        if table == Ordering::Equal {
            compare(&query.1, &stored.1, self.0).await
        } else {
            Ok(table)
        }
    }
}

async fn compare(
    left: &str,
    right: &str,
    input: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<Ordering> {
    for (left, right) in left
        .as_bytes()
        .chunks(4096)
        .zip(right.as_bytes().chunks(4096))
    {
        input.checkpoint().await?;
        let order = left.cmp(right);
        if order != Ordering::Equal {
            return Ok(order);
        }
    }
    input.checkpoint().await?;
    Ok(left.len().cmp(&right.len()))
}

#[cfg(test)]
mod tests;
