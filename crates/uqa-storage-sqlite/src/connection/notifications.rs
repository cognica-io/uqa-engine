//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The connection retains the session gate while shared storage stages or recovers publication.

mod serialized;

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
        if self.session.logical.get().is_none() {
            return serialized::stage(self, publication, None);
        }
        self.with_notification_publications(|store| {
            store.stage_notification_publication(publication)
        })
    }

    fn stage_notification_publication_after(
        &self,
        publication: &NotificationPublication,
        acknowledged: Option<[u8; 32]>,
    ) -> StorageBackendResult<()> {
        if self.session.logical.get().is_none() {
            return serialized::stage(self, publication, acknowledged);
        }
        self.with_notification_publications(|store| {
            store.stage_notification_publication_after(publication, acknowledged)
        })
    }

    fn visit_notification_publication(
        &self,
        control: &StorageReadControl,
        visit: &mut dyn FnMut(Option<NotificationPublicationView<'_>>) -> StorageBackendResult<()>,
    ) -> StorageBackendResult<()> {
        if self.session.logical.get().is_none() {
            return serialized::visit(self, control, visit);
        }
        self.with_notification_publications(|store| {
            store.visit_notification_publication(control, visit)
        })
    }

    fn acknowledge_notification_publication(
        &self,
        fingerprint: [u8; 32],
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        if self.session.logical.get().is_none() {
            if serialized::acknowledge(self, fingerprint, control, false)? {
                return Ok(());
            }
            unreachable!("blocking acknowledgement either completes or returns an error");
        }
        self.with_notification_publications(|store| {
            store.acknowledge_notification_publication(fingerprint, control)
        })
    }

    fn try_acknowledge_notification_publication(
        &self,
        fingerprint: [u8; 32],
        control: &StorageReadControl,
    ) -> StorageBackendResult<bool> {
        if self.session.logical.get().is_none() {
            return serialized::acknowledge(self, fingerprint, control, true);
        }
        self.with_notification_publications(|store| {
            store.try_acknowledge_notification_publication(fingerprint, control)
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
