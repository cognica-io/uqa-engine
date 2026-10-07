//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind physical filter leaves to the shared query index reconciliation.

use uqa_core::{PostingList, Predicate};
use uqa_sql::SQLError;

impl super::PhysicalRetrievalDriver<'_> {
    pub(super) fn value_index_scan(
        &self,
        field: &str,
        predicate: &Predicate,
    ) -> Result<Option<PostingList>, SQLError> {
        crate::query::indexed_filter::scan(
            self.context.indexes,
            self.table,
            field,
            predicate,
            &self.context.runtime.cancellation_token(),
        )
    }
}
