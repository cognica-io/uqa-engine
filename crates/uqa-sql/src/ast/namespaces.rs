//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema authorization preserves literal role names separately from session-role keywords.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SchemaAuthorization {
    Role(String),
    CurrentUser,
    SessionUser,
}
