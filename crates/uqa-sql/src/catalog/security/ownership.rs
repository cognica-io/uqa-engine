//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read-only schema authority shared by relation ownership checks.
use super::SchemaSecurity;
pub trait RelationOwnerSchemas {
    fn schema_security(&self, schema: &str) -> Option<SchemaSecurity>;
}
