//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind INSERT SELECT services to the caller's statement generation.
use crate::{session::StatementReadSnapshot, Engine};
use uqa_execution::mutation::insert::source::InsertSourceContext;
impl Engine {
    pub(crate) fn insert_source_context(&self) -> InsertSourceContext<'_, StatementReadSnapshot> {
        InsertSourceContext {
            rows: self.mutation_preparation_context(),
            identities: self.insert_identity_context(),
            runtime: self.query_runtime_view(),
        }
    }
}
