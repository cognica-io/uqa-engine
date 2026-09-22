//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained physical index snapshots preserve the common read-only contract.

use std::sync::Arc;

use crate::{ReadOnlySnapshot, VectorIndex};

pub(in crate::key_value) fn read_only(index: Arc<dyn VectorIndex>) -> Arc<dyn VectorIndex> {
    Arc::new(ReadOnlySnapshot::new(index))
}
