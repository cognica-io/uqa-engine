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
    pub(super) fn mutate_persistent_value<T>(
        &self,
        temporary: bool,
        action: &str,
        operation: impl Fn(&dyn CatalogFacade) -> StorageBackendResult<T>,
    ) -> Result<Option<(T, bool)>, SequenceValueError> {
        if temporary {
            return Ok(None);
        }
        let session = self
            .runtime
            .open_nontransactional_sequence_session()
            .map_err(|error| sequence_storage_error("open sequence session", error))?;
        if let Some(session) = session {
            return autonomous_value(&session, self.runtime.cancellation(), &operation)
                .map(|value| Some((value, true)))
                .map_err(|error| sequence_storage_error(action, error));
        }
        self.runtime
            .prepare_explicit_transaction_writer()
            .map_err(|error| match error {
                error @ uqa_sql::SQLError::Internal(_) => {
                    SequenceValueError::Internal(format!("prepare sequence writer: {error}"))
                }
                error => error.into(),
            })?;
        self.storage
            .map(|catalog| operation(catalog).map(|value| (value, false)))
            .transpose()
            .map_err(|error| sequence_storage_error(action, error))
    }
}

fn autonomous_value<T>(
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

fn sequence_storage_error(action: &str, error: StorageBackendError) -> SequenceValueError {
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
