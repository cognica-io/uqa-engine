//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical schema changes and validation of stored rows.
pub mod columns;
pub mod ctas;
pub mod indexes;
pub mod validation;

pub mod sequences;

pub mod publication;

pub mod table_creation;

pub mod hierarchy;

pub mod keys;

pub mod constraints;

pub mod events;

pub mod table_alteration;

pub mod removal;

pub mod relation_alteration;

pub mod view_alteration;
pub mod view_creation;
pub mod view_dependencies;

pub mod domains;
pub mod foreign_table_alteration;
pub mod namespaces;

pub mod view_references;
pub mod view_removal;

pub mod truncate;

pub mod foreign_creation;

pub mod foreign_definitions;

pub mod foreign_removal;

pub mod table_removal;
