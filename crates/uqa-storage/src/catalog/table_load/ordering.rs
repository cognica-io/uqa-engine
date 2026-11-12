//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::CatalogTable;
use crate::catalog::ordering::compare_names as compare;
use crate::{StorageBackendError, StorageBackendResult};
use std::cmp::Ordering;
use uqa_core::{
    image_io::AsyncImageInput,
    ordering::{sort_by_with_progress, SortProgress},
};

struct Progress<'a, 'b>(&'a mut AsyncImageInput<'b>);

impl SortProgress<CatalogTable, StorageBackendError> for Progress<'_, '_> {
    async fn checkpoint(&mut self) -> StorageBackendResult<()> {
        Ok(self.0.checkpoint().await?)
    }

    async fn compare(
        &mut self,
        left: &CatalogTable,
        right: &CatalogTable,
    ) -> StorageBackendResult<Ordering> {
        let order = compare(&left.relation.schema, &right.relation.schema, self.0).await?;
        if order == Ordering::Equal {
            compare(&left.relation.name, &right.relation.name, self.0).await
        } else {
            Ok(order)
        }
    }
}

pub(super) async fn sort(
    tables: &mut [CatalogTable],
    progress: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<()> {
    sort_by_with_progress(tables, &mut Progress(progress)).await
}
