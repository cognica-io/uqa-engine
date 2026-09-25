//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Notification staging shares the data outcome; recovery and acknowledgement use fresh autonomous state.

use super::{no_transaction, Transaction, VersionedKeyValueStore};
use crate::{
    mvcc::{notifications::NotificationEffect, VersionError},
    notifications::{
        NotificationPublication, NotificationPublicationStore, NotificationPublicationView,
    },
    read_control::StorageReadControl,
    StorageBackendResult,
};

impl NotificationPublicationStore for VersionedKeyValueStore {
    fn stage_notification_publication(
        &self,
        publication: &NotificationPublication,
    ) -> StorageBackendResult<()> {
        self.require_mutable_session()?;
        let control = self.write_control();
        let effect = NotificationEffect::publish(
            publication,
            self.persistence.notification_record_layout(),
            &control,
        )
        .map_err(VersionError::into_storage_error)?;
        self.active
            .lock()
            .as_mut()
            .ok_or_else(no_transaction)?
            .stage_notification(effect)
            .map_err(VersionError::into_storage_error)
    }

    fn visit_notification_publication(
        &self,
        control: &StorageReadControl,
        visit: &mut dyn FnMut(Option<NotificationPublicationView<'_>>) -> StorageBackendResult<()>,
    ) -> StorageBackendResult<()> {
        control.check()?;
        let layout = self.persistence.notification_record_layout();
        let key = layout
            .key(control)
            .map_err(VersionError::into_storage_error)?;
        let snapshot = self
            .persistence
            .snapshot(control)
            .map_err(VersionError::into_storage_error)?;
        snapshot
            .visit_value(&key, control, &mut |record| {
                let publication = record
                    .and_then(|record| record.value)
                    .map(|value| layout.decode(&key, value, control))
                    .transpose()?;
                visit(publication).map_err(Into::into)
            })
            .map_err(VersionError::into_storage_error)?;
        control.check()
    }

    fn acknowledge_notification_publication(
        &self,
        fingerprint: [u8; 32],
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        self.require_mutable_session()?;
        control.check()?;
        let mut pending = false;
        self.visit_notification_publication(control, &mut |publication| {
            pending =
                publication.is_some_and(|publication| publication.fingerprint() == fingerprint);
            Ok(())
        })?;
        if !pending {
            return Ok(());
        }
        let effect = NotificationEffect::acknowledge(
            fingerprint,
            self.persistence.notification_record_layout(),
            control,
        )
        .map_err(VersionError::into_storage_error)?;
        let mut transaction = Transaction::new(&*self.persistence, false, control)
            .map_err(VersionError::into_storage_error)?;
        transaction
            .stage_notification(effect)
            .map_err(VersionError::into_storage_error)?;
        transaction.commit(&*self.persistence, control)?;
        transaction.acknowledge_completion(&*self.persistence, control)
    }
}
