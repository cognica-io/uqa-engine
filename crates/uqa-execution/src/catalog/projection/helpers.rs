//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Responsibility-owned helpers for `PostgreSQL` catalog projection.

pub mod acl;
pub mod constraints;
mod dependencies;
pub mod index_definitions;
pub mod information_schema_types;
pub mod oids;
pub mod rows;
pub use uqa_sql::catalog::type_metadata;
pub mod views;
