//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Only rejected autonomous value mutations may be evaluated again; bound transactions and uncertain outcomes are never replayed.

use super::{NextvalTarget, SequenceValueContext, SequenceValueError};
use uqa_storage::{
    CatalogFacade, PersistentStorageSession, StorageBackendError, StorageBackendResult,
};

pub(super) enum SequenceValueStorage<'a> {
    Memory,
    Bound(&'a dyn CatalogFacade),
    Autonomous(PersistentStorageSession),
    Positioned {
        session: PersistentStorageSession,
        positions: &'a crate::row_locks::RowLockManager,
    },
}

impl SequenceValueStorage<'_> {
    pub(super) fn persistent(&self) -> bool {
        !matches!(self, Self::Memory)
    }
    pub(super) fn mutate<T>(
        &self,
        cancellation: &uqa_core::CancellationToken,
        action: &str,
        operation: impl Fn(&dyn CatalogFacade) -> StorageBackendResult<T>,
    ) -> Result<Option<(T, bool)>, SequenceValueError> {
        match self {
            Self::Memory => Ok(None),
            Self::Bound(catalog) => operation(*catalog).map(|value| Some((value, false))),
            Self::Autonomous(session) | Self::Positioned { session, .. } => {
                autonomous_value(session, cancellation, &operation).map(|value| Some((value, true)))
            }
        }
        .map_err(|error| sequence_storage_error(action, error))
    }
}

impl SequenceValueContext<'_> {
    /// Whether the caller's transaction created this definition generation, whose value record no other session can see, so that its values are allocated in the transaction. The values of a committed generation are allocated outside any transaction, even when the transaction changed the sequence's privileges, owner, name or ownership, as `PostgreSQL` moves a sequence's state outside the transaction that changes its catalog rows.
    pub(super) fn sequence_value_is_private(
        &self,
        temporary: bool,
        object_id: [u8; 16],
        definition_generation: [u8; 16],
    ) -> Result<bool, SequenceValueError> {
        if temporary {
            return Ok(false);
        }
        Ok(self
            .storage
            .map(|catalog| {
                catalog.sequence_value_has_private_changes(object_id, definition_generation)
            })
            .transpose()
            .map_err(|error| sequence_storage_error("inspect sequence transaction scope", error))?
            .unwrap_or(false))
    }

    /// Choose the physical session and admit the explicit writer before acquiring native catalog or history publication guards.
    pub(super) fn value_storage(
        &self,
        target: &NextvalTarget,
    ) -> Result<SequenceValueStorage<'_>, SequenceValueError> {
        if target.temporary {
            return Ok(SequenceValueStorage::Memory);
        }
        let private = self.sequence_value_is_private(
            target.temporary,
            target.object_id,
            target.state.definition_generation,
        )?;
        if let Some(positions) = self.shared_positions(target, private) {
            let session = self
                .runtime
                .open_nontransactional_sequence_session()
                .map_err(|error| sequence_storage_error("open sequence session", error))?
                .ok_or_else(|| {
                    SequenceValueError::Internal(
                        "sequence positions require an independent catalog session".into(),
                    )
                })?;
            return Ok(SequenceValueStorage::Positioned { session, positions });
        }
        if !private {
            if let Some(session) = self
                .runtime
                .open_nontransactional_sequence_session()
                .map_err(|error| sequence_storage_error("open sequence session", error))?
            {
                return Ok(SequenceValueStorage::Autonomous(session));
            }
        }
        self.prepare_transaction_writer()?;
        Ok(self
            .storage
            .map_or(SequenceValueStorage::Memory, SequenceValueStorage::Bound))
    }

    /// Make the caller's transaction a writer before a sequence value is changed in it.
    pub(super) fn prepare_transaction_writer(&self) -> Result<(), SequenceValueError> {
        self.runtime
            .prepare_explicit_transaction_writer()
            .map_err(|error| match error {
                error @ uqa_sql::SQLError::Internal(_) => {
                    SequenceValueError::Internal(format!("prepare sequence writer: {error}"))
                }
                error => error.into(),
            })
    }
}

pub(super) fn autonomous_value<T>(
    session: &PersistentStorageSession,
    cancel: &uqa_core::CancellationToken,
    operation: &impl Fn(&dyn CatalogFacade) -> StorageBackendResult<T>,
) -> StorageBackendResult<T> {
    session.validate_transaction_affinity()?;
    if session.backend.in_transaction() {
        return Err(StorageBackendError::Other(
            "autonomous sequence values require an idle independent session".into(),
        ));
    }
    loop {
        cancel.check()?;
        match operation(session.catalog.as_ref()) {
            Err(error) if rejected_value_conflict(&error) => {
                if session.backend.in_transaction() {
                    session.backend.rollback_transaction()?;
                }
            }
            result => return result,
        }
    }
}

fn rejected_value_conflict(error: &StorageBackendError) -> bool {
    let StorageBackendError::Backend { source, .. } = error else {
        return false;
    };
    matches!(
        source.downcast_ref::<uqa_storage::mvcc::VersionError>(),
        Some(
            uqa_storage::mvcc::VersionError::WriteConflict { .. }
                | uqa_storage::mvcc::VersionError::ReadConflict { .. }
        )
    )
}

pub(super) fn sequence_storage_error(
    action: &str,
    error: StorageBackendError,
) -> SequenceValueError {
    match crate::storage_errors::storage_error(action, &error) {
        uqa_sql::SQLError::Internal(_) => {
            SequenceValueError::Internal(format!("{action}: {error}"))
        }
        error => error.into(),
    }
}

#[cfg(test)]
mod tests;
