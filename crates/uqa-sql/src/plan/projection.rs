//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

/// Identity assigned to one projection result. SQL columns participate in
/// ordinary name binding and wildcard expansion; internal attributes are
/// executor-only `resjunk` slots addressed structurally.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ProjectionTarget {
    Column(String),
    Internal(crate::ast::InternalColumnRef),
}

impl From<String> for ProjectionTarget {
    fn from(value: String) -> Self {
        Self::Column(value)
    }
}

impl From<&str> for ProjectionTarget {
    fn from(value: &str) -> Self {
        Self::Column(value.to_string())
    }
}
