//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog publication, guards, and durable Engine state.

mod alteration_authority;
mod events;
mod foreign_tables;
mod hierarchy_restoration;
mod index_drop_binding;
mod index_identities;
mod relations;
mod roles;
mod routines;
mod schemas;
mod sequences;
mod table_alteration;
mod table_alteration_locks;
mod table_authorization;
mod table_grants;
mod table_ownership;

mod table_removal;
