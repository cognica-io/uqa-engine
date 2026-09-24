//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Queue publication and its bounded acknowledgement share one registry transaction.

use super::{
    fixed_bytes, nonnegative_u64, registry_error, sqlite_integer, NotificationListenerRow,
    NotificationRegistry, NotificationRegistryTransaction, StorageBackendError,
    StorageBackendResult,
};
use rusqlite::{params, Connection};
use uqa_storage::{
    mvcc::VersionError,
    notifications::{
        NotificationPublication, NotificationPublicationStart, NotificationPublicationStore,
        NotificationPublicationView, PendingNotification,
    },
    read_control::StorageReadControl,
};

struct PublicationState {
    registry_id: [u8; 16],
    next_publication: u64,
    acknowledged: Option<[u8; 32]>,
}

fn state(connection: &Connection) -> StorageBackendResult<PublicationState> {
    let (registry_id, next, acknowledged) = connection.query_row(
        "SELECT registry_id, next_publication, acknowledged_fingerprint FROM publication_state WHERE singleton = 1", [],
        |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?, row.get::<_, Option<Vec<u8>>>(2)?)),
    ).map_err(|error| registry_error("read publication state", &error))?;
    Ok(PublicationState {
        registry_id: fixed_bytes(registry_id, "registry identity")?,
        next_publication: nonnegative_u64(next, "publication sequence")?,
        acknowledged: acknowledged
            .map(|value| fixed_bytes(value, "publication acknowledgement"))
            .transpose()?,
    })
}

impl NotificationRegistry {
    /// Recover any committed slot before admitting another publisher. Each recovery commits its queue acknowledgement before conditionally clearing the main-store slot. A concurrent cleanup or publisher cannot make a stale acknowledgement delete a newer intent.
    pub fn begin_recovered(
        &self,
        store: &dyn NotificationPublicationStore,
        control: &StorageReadControl,
    ) -> StorageBackendResult<NotificationRegistryTransaction> {
        loop {
            control.check()?;
            let mut transaction = self.begin()?;
            let mut recovered = None;
            store.visit_notification_publication(control, &mut |publication| {
                if let Some(publication) = publication {
                    transaction.apply_publication(publication, control)?;
                    recovered = Some(publication.fingerprint());
                }
                Ok(())
            })?;
            let Some(fingerprint) = recovered else {
                return Ok(transaction);
            };
            transaction.commit()?;
            store.acknowledge_notification_publication(fingerprint, control)?;
        }
    }
}

impl NotificationRegistryTransaction {
    /// Prepare the original evaluated batch and final subscription under registry admission. The caller stages the returned immutable record in its main transaction before committing either resource.
    pub fn prepare_publication(
        &mut self,
        process_id: i32,
        pending: &[PendingNotification],
        listener: Option<&NotificationListenerRow>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<NotificationPublication> {
        control.check()?;
        let state = state(&self.connection)?;
        let queue = self.queue_state()?;
        let publication = NotificationPublication::encode(
            NotificationPublicationStart {
                registry_id: state.registry_id,
                publication_sequence: state.next_publication,
                first_sequence: queue.next_sequence,
                first_position: queue.head_position,
                process_id,
            },
            pending,
            listener,
            control,
        )
        .map_err(VersionError::into_storage_error)?;
        self.apply_publication(publication.view(), control)?;
        Ok(publication)
    }

    /// Append exactly the authoritative committed intent, or recognize its already committed acknowledgement. A savepoint prevents failed decoding, cancellation or memory admission from leaving half a publication in this registry transaction.
    pub fn apply_publication(
        &mut self,
        publication: NotificationPublicationView<'_>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        control.check()?;
        let mut savepoint = self
            .connection
            .savepoint()
            .map_err(|error| registry_error("begin publication savepoint", &error))?;
        if let Err(error) = apply(&savepoint, publication, control) {
            if let Err(rollback) = savepoint.rollback().and_then(|()| savepoint.commit()) {
                self.poisoned = true;
                return Err(StorageBackendError::Other(format!(
                    "{error}; rollback notification publication failed: {rollback}"
                )));
            }
            return Err(error);
        }
        savepoint
            .commit()
            .map_err(|error| registry_error("finish publication savepoint", &error))
    }
}

fn apply(
    connection: &Connection,
    publication: NotificationPublicationView<'_>,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let header = publication.header();
    let state = state(connection)?;
    let fingerprint = publication.fingerprint();
    let queue = connection
        .query_row(
            "SELECT next_sequence, head_position FROM queue_state WHERE singleton = 1",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .map_err(|error| registry_error("read publication queue boundary", &error))?;
    let queue = (
        nonnegative_u64(queue.0, "queue sequence")?,
        nonnegative_u64(queue.1, "queue position")?,
    );
    if state.registry_id != header.registry_id {
        return Err(StorageBackendError::Other(
            "committed notification belongs to a different registry incarnation".into(),
        ));
    }
    if state.acknowledged == Some(fingerprint) {
        if state.next_publication != header.publication_sequence + 1
            || queue != (header.next_sequence, header.end_position)
        {
            return Err(StorageBackendError::Other(
                "notification acknowledgement has an inconsistent queue boundary".into(),
            ));
        }
        return Ok(());
    }
    if state.next_publication != header.publication_sequence
        || queue != (header.first_sequence, header.first_position)
    {
        return Err(StorageBackendError::Other(
            "committed notification conflicts with the registry publication boundary".into(),
        ));
    }
    apply_subscription(connection, publication, control)?;
    let mut insert = connection.prepare_cached("INSERT INTO queue_entries (sequence, process_id, channel, payload) VALUES (?1, ?2, ?3, ?4)")
        .map_err(|error| registry_error("prepare recovered queue append", &error))?;
    for (sequence, message) in
        (header.first_sequence..header.next_sequence).zip(publication.messages())
    {
        control.check()?;
        let message = message.map_err(VersionError::into_storage_error)?;
        insert
            .execute(params![
                sqlite_integer(sequence, "recovered entry sequence")?,
                header.process_id,
                message.channel,
                message.payload
            ])
            .map_err(|error| registry_error("append recovered queue entry", &error))?;
    }
    control.check()?;
    connection
        .execute(
            "UPDATE queue_state SET next_sequence = ?1, head_position = ?2 WHERE singleton = 1",
            params![
                sqlite_integer(header.next_sequence, "queue sequence")?,
                sqlite_integer(header.end_position, "queue position")?
            ],
        )
        .map_err(|error| registry_error("advance recovered queue state", &error))?;
    connection.execute("UPDATE publication_state SET next_publication = ?1, acknowledged_fingerprint = ?2 WHERE singleton = 1", params![sqlite_integer(header.publication_sequence + 1, "publication sequence")?, fingerprint.as_slice()])
        .map_err(|error| registry_error("acknowledge recovered publication", &error))?;
    Ok(())
}

fn apply_subscription(
    connection: &Connection,
    publication: NotificationPublicationView<'_>,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let header = publication.header();
    if let Some(subscription) = publication.subscription() {
        control.check()?;
        if subscription.is_unlisten() {
            connection
                .execute(
                    "DELETE FROM listeners WHERE owner_id = ?1 AND session_id = ?2",
                    params![
                        subscription.owner_id.as_slice(),
                        subscription.session_id.to_be_bytes().as_slice()
                    ],
                )
                .map_err(|error| registry_error("apply committed unlisten", &error))?;
        } else {
            let channels = subscription
                .channels_json(control)
                .map_err(VersionError::into_storage_error)?;
            let channels = std::str::from_utf8(&channels).map_err(|_| {
                StorageBackendError::Other("invalid encoded notification channels".into())
            })?;
            connection.execute(
                "INSERT INTO listeners (owner_id, session_id, process_id, wake_port, channels_json, transaction_open, next_sequence, position) VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7) ON CONFLICT(owner_id, session_id) DO UPDATE SET process_id = excluded.process_id, wake_port = excluded.wake_port, channels_json = excluded.channels_json, transaction_open = 0, next_sequence = excluded.next_sequence, position = excluded.position",
                params![subscription.owner_id.as_slice(), subscription.session_id.to_be_bytes().as_slice(), header.process_id, i64::from(subscription.wake_port), channels, sqlite_integer(subscription.next_sequence, "listener sequence")?, sqlite_integer(subscription.position, "listener position")?],
            ).map_err(|error| registry_error("apply committed listen", &error))?;
        }
    }
    Ok(())
}
