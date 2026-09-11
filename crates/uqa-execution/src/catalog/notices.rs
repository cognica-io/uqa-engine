//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed notice delivery for catalog operations.
pub trait CatalogNotices {
    fn notice(&self, level: &str, message: &str);
}
