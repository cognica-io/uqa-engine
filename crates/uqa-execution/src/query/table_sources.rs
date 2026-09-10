//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical table-source construction over independently owned read and retrieval services.
use super::{runtime::QueryRuntimeView, table_read::QueryTableAccess};
pub mod hierarchy;
pub mod retrieval;
pub mod scan;
#[derive(Clone, Copy)]
pub struct TableScanContext<'a> {
    pub tables: &'a dyn QueryTableAccess,
    pub runtime: QueryRuntimeView<'a>,
}
#[derive(Clone, Copy)]
pub struct TableRetrievalContext<'a> {
    pub tables: &'a dyn QueryTableAccess,
    pub retrieval: &'a dyn retrieval::RetrievalAccess,
}
