//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Committed row-change metadata carried to transaction publication.

#[derive(Clone, Copy)]
pub struct TransactionRowChange {
    pub pending: crate::row_locks::PendingRowChange,
    pub source_generation: [u8; 16],
    pub successor_generation: Option<[u8; 16]>,
    pub query_origin: Option<u64>,
}
