//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The connection retains the session gate while shared storage stages or recovers publication.

use super::ManagedConnection;
use uqa_storage::{
    notifications::{
        NotificationPublication, NotificationPublicationStore, NotificationPublicationView,
    },
    read_control::StorageReadControl,
    StorageBackendResult,
};

impl NotificationPublicationStore for ManagedConnection {
    fn stage_notification_publication(
        &self,
        publication: &NotificationPublication,
    ) -> StorageBackendResult<()> {
        self.with_notification_publications(|store| {
            store.stage_notification_publication(publication)
        })
    }

    fn visit_notification_publication(
        &self,
        control: &StorageReadControl,
        visit: &mut dyn FnMut(Option<NotificationPublicationView<'_>>) -> StorageBackendResult<()>,
    ) -> StorageBackendResult<()> {
        self.with_notification_publications(|store| {
            store.visit_notification_publication(control, visit)
        })
    }

    fn acknowledge_notification_publication(
        &self,
        fingerprint: [u8; 32],
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        self.with_notification_publications(|store| {
            store.acknowledge_notification_publication(fingerprint, control)
        })
    }
}

impl ManagedConnection {
    fn with_notification_publications<T>(
        &self,
        operation: impl FnOnce(&dyn NotificationPublicationStore) -> StorageBackendResult<T>,
    ) -> StorageBackendResult<T> {
        self.surface_cleanup_failure()?;
        let _gate = self.session.gate.read();
        let logical = self
            .session
            .logical
            .get()
            .ok_or(crate::SQLiteError::LogicalSessionRequired)?;
        operation(logical.store.as_ref())
    }
}
