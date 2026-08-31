//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Responsibility-owned helpers for `PostgreSQL` catalog projection.

pub(super) mod constraints;
mod dependencies;
pub(super) mod index_definitions;
pub(super) mod information_schema_types;
pub(super) mod oids;
pub(super) mod rows;
pub(super) mod type_metadata;
pub(super) mod views;
