//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Only rejected autonomous value mutations may be evaluated again; bound transactions and uncertain outcomes are never replayed.

use super::{SequenceValueContext, SequenceValueError};
use uqa_storage::{
    CatalogFacade, PersistentStorageSession, StorageBackendError, StorageBackendResult,
};

impl SequenceValueContext<'_> {
    /// Whether the caller's transaction holds changes of this sequence that an independent session would not see.
    pub(super) fn sequence_is_private(
        &self,
        temporary: bool,
        relation: &uqa_core::RelationIdentity,
        object_id: [u8; 16],
    ) -> Result<bool, SequenceValueError> {
        if temporary {
            return Ok(false);
        }
        Ok(self
            .storage
            .map(|catalog| catalog.sequence_has_private_changes(relation, object_id))
            .transpose()
            .map_err(|error| sequence_storage_error("inspect sequence transaction scope", error))?
            .unwrap_or(false))
    }

    pub(super) fn mutate_persistent_value<T>(
        &self,
        temporary: bool,
        private: bool,
        action: &str,
        operation: impl Fn(&dyn CatalogFacade) -> StorageBackendResult<T>,
    ) -> Result<Option<(T, bool)>, SequenceValueError> {
        if temporary {
            return Ok(None);
        }
        let session = if private {
            None
        } else {
            self.runtime
                .open_nontransactional_sequence_session()
                .map_err(|error| sequence_storage_error("open sequence session", error))?
        };
        if let Some(session) = session {
            return autonomous_value(&session, self.runtime.cancellation(), &operation)
                .map(|value| Some((value, true)))
                .map_err(|error| sequence_storage_error(action, error));
        }
        self.prepare_transaction_writer()?;
        self.storage
            .map(|catalog| operation(catalog).map(|value| (value, false)))
            .transpose()
            .map_err(|error| sequence_storage_error(action, error))
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
    match error {
        StorageBackendError::Cancelled(error) => SequenceValueError::Cancelled(error),
        StorageBackendError::Memory(error) => uqa_sql::SQLError::Routine {
            sqlstate: "53200".into(),
            message: format!("{action}: {error}"),
        }
        .into(),
        error => SequenceValueError::Internal(format!("{action}: {error}")),
    }
}

#[cfg(test)]
mod tests;
