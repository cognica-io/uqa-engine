//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete stored statements preserve declarations until their owning semantic checks.

use super::{invalid, Input, InputError, JsonToken, Native, Parts, Stored, Values};

mod composites;
mod constraints;
mod definitions;
mod domains;
mod enums;
mod events;
mod foreign;
mod foreign_wrappers;
mod identity_sequence;
mod indexes;
mod locking;
mod namespaces;
mod primitives;
mod referenced_partition;
mod relation_hierarchy;
mod relation_lifecycle;
mod roles;
mod routine_attributes;
mod routine_security;
mod routines;
mod sequence;
mod sequence_declaration;
mod special;
mod statements;
mod table_alteration;
mod type_lifecycle;
mod type_privileges;

mod api;

#[cfg(test)]
mod tests;
