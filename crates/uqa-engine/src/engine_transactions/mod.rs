//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    BTreeSet, BackendTransactionMode, ConstraintModeState, Engine, EngineDataSnapshot,
    FixedTransactionSnapshot, NontransactionalColumnStats, NontransactionalSequenceValues,
    SQLError, SQLParam, SQLResult, SessionLastSequenceReference, SessionStateSnapshot,
    StorageBackendError, StorageBackendResult, StorageSavepointId, TransactionCharacteristicsState,
    TransactionDirtyState, TransactionFrame, TransactionIntent, TransactionRelationStates,
    TransactionRowChange, TransactionSavepoint, TransactionStatus,
};

mod backend;
mod characteristics;
mod control;
mod coordinator;
mod failure;
use failure::{failed_transaction_error, panic_description};
mod frame_finish;
mod implicit;
mod publication;
mod savepoints;
mod scope;
use scope::TransactionScope;
mod snapshots;

mod constraints;
pub(crate) use constraints::constraint_identities_match;
mod row_locks_session;

#[cfg(test)]
mod tests;
