//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Role references of the catalog: the retained registries that name roles, and the roles that session-local relations reference. `DROP ROLE` reports what depends on a role from the catalog's shared dependencies.

pub mod context;
pub mod temporary;

#[cfg(test)]
mod tests;
