//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared mutation identities, prepared actions, locking, event state, and command scopes.

mod candidate;
mod command;
mod deferred;
mod publication;

pub(in crate::sql) use candidate::PhysicalDocumentIdentity;
pub(in crate::sql) use command::run_mutation_command;
pub(crate) use command::{CommandExactIndex, CommandMutationOverlay, CommandStoredDocument};
pub(crate) use deferred::DeferredForeignKeyCheck;
pub(crate) use publication::TransactionRowChange;
