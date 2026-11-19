//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete physical analyzer families retain their original tuple order.

use super::{CatalogAnalyzerBindingRow, CatalogAnalyzerLabelRow};
use crate::{catalog::ordering::compare_names, StorageBackendError, StorageBackendResult};
use std::cmp::Ordering;
use uqa_core::{
    image_io::AsyncImageInput,
    memory::Budgeted,
    ordering::{sort_by_with_progress, SortProgress},
};

impl CatalogAnalyzerBindingRow {
    /// Order complete original binding tuples before canonical names or envelope semantics are interpreted.
    pub async fn order_from(
        rows: &mut [Budgeted<Self>],
        input: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<()> {
        sort_by_with_progress(
            rows,
            &mut Fields {
                input,
                fields: |row: &Self| {
                    [
                        row.table.as_str(),
                        row.field.as_str(),
                        row.binding_json.as_str(),
                    ]
                },
            },
        )
        .await
    }
}

impl CatalogAnalyzerLabelRow {
    /// Order complete original label tuples, including phase and analyzer name, before canonical identity grouping.
    pub async fn order_from(
        rows: &mut [Budgeted<Self>],
        input: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<()> {
        sort_by_with_progress(
            rows,
            &mut Fields {
                input,
                fields: |row: &Self| {
                    [
                        row.table.as_str(),
                        row.field.as_str(),
                        row.phase.as_str(),
                        row.analyzer_name.as_str(),
                    ]
                },
            },
        )
        .await
    }
}

struct Fields<'a, 'c, T, const N: usize> {
    input: &'a mut AsyncImageInput<'c>,
    fields: fn(&T) -> [&str; N],
}

impl<T, const N: usize> SortProgress<Budgeted<T>, StorageBackendError> for Fields<'_, '_, T, N> {
    async fn checkpoint(&mut self) -> StorageBackendResult<()> {
        self.input.checkpoint().await?;
        Ok(())
    }

    async fn compare(
        &mut self,
        left: &Budgeted<T>,
        right: &Budgeted<T>,
    ) -> StorageBackendResult<Ordering> {
        for (left, right) in (self.fields)(left).into_iter().zip((self.fields)(right)) {
            let order = compare_names(left, right, self.input).await?;
            if order != Ordering::Equal {
                return Ok(order);
            }
        }
        Ok(Ordering::Equal)
    }
}
