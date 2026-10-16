//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Pinned type metadata for observing admitted range payloads without invoking input or canonicalization.

use crate::ColumnType;

#[derive(Debug, Clone)]
pub struct RangeValueDescriptor {
    pub range_oid: u32,
    pub multirange_oid: u32,
    pub endpoint: ColumnType,
}
