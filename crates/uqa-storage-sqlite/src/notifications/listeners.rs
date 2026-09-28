//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Indexed, one-row coordination reads without hydrating unrelated channel lists.

use rusqlite::{params, OptionalExtension, Row};
use serde::de::{self, DeserializeSeed, SeqAccess, Visitor};
use uqa_storage::{
    notifications::{
        NotificationListenerKey, NotificationListenerMetadata, NotificationListenerRow,
        MAX_NOTIFICATION_CHANNEL_BYTES,
    },
    StorageBackendError, StorageBackendResult,
};

use super::{control, nonnegative_u64, registry_error, NotificationRegistryTransaction};

impl NotificationRegistryTransaction {
    /// Read one fixed-width row after the exclusive key in primary-key order. The statement finishes before return, so deleting that row and resuming cannot invalidate an active scan. Channel JSON is not selected or decoded. The original transaction's cancellation still applies.
    pub fn listener_metadata_after(
        &self,
        after: Option<NotificationListenerKey>,
    ) -> StorageBackendResult<Option<NotificationListenerMetadata>> {
        let _operation = control::operation(&self.connection, self.control.as_ref())?;
        let result = if let Some(after) = after {
            self.connection.prepare_cached(
                "SELECT owner_id, session_id, process_id, wake_port, transaction_open, next_sequence, position FROM listeners WHERE (owner_id, session_id) > (?1, ?2) ORDER BY owner_id, session_id LIMIT 1",
            ).map_err(|error| registry_error("prepare listener metadata resume", &error))?
                .query_row(params![after.owner_id.as_slice(), after.session_id.to_be_bytes().as_slice()], |row| Ok(metadata(row)))
        } else {
            self.connection.prepare_cached(
                "SELECT owner_id, session_id, process_id, wake_port, transaction_open, next_sequence, position FROM listeners ORDER BY owner_id, session_id LIMIT 1",
            ).map_err(|error| registry_error("prepare listener metadata scan", &error))?
                .query_row([], |row| Ok(metadata(row)))
        }.optional().map_err(|error| registry_error("read listener metadata", &error))?.transpose()?;
        if let Some(control) = self.control.as_ref() {
            control.check()?;
        }
        Ok(result)
    }

    /// Load only the named listener. A supplied channel limit bounds encoded JSON before copying and decoded element count before constructing excess elements. `None` preserves explicit legacy session materialization.
    pub fn listener(
        &self,
        key: NotificationListenerKey,
        max_channels: Option<usize>,
    ) -> StorageBackendResult<Option<NotificationListenerRow>> {
        let _operation = control::operation(&self.connection, self.control.as_ref())?;
        let result = self.connection.prepare_cached(
            "SELECT owner_id, session_id, process_id, wake_port, transaction_open, next_sequence, position, channels_json FROM listeners WHERE owner_id = ?1 AND session_id = ?2",
        ).map_err(|error| registry_error("prepare listener lookup", &error))?
            .query_row(params![key.owner_id.as_slice(), key.session_id.to_be_bytes().as_slice()], |row| Ok(read_listener(row, max_channels)))
            .optional().map_err(|error| registry_error("read listener", &error))?.transpose()?;
        if let Some(control) = self.control.as_ref() {
            control.check()?;
        }
        Ok(result)
    }
}

fn fixed<const N: usize>(row: &Row<'_>, column: usize) -> StorageBackendResult<[u8; N]> {
    row.get_ref(column)
        .and_then(|value| value.as_blob().map_err(rusqlite::Error::from))
        .map_err(|error| registry_error("read listener identity", &error))?
        .try_into()
        .map_err(|_| {
            StorageBackendError::Other("corrupt asynchronous notification listener identity".into())
        })
}

fn metadata(row: &Row<'_>) -> StorageBackendResult<NotificationListenerMetadata> {
    let process_id = row
        .get(2)
        .map_err(|error| registry_error("read listener process", &error))?;
    let port: i64 = row
        .get(3)
        .map_err(|error| registry_error("read listener port", &error))?;
    let transaction_open = row
        .get(4)
        .map_err(|error| registry_error("read listener transaction state", &error))?;
    let sequence = row
        .get(5)
        .map_err(|error| registry_error("read listener sequence", &error))?;
    let position = row
        .get(6)
        .map_err(|error| registry_error("read listener position", &error))?;
    Ok(NotificationListenerMetadata {
        key: NotificationListenerKey {
            owner_id: fixed(row, 0)?,
            session_id: u64::from_be_bytes(fixed(row, 1)?),
        },
        process_id,
        wake_port: u16::try_from(port).map_err(|_| {
            StorageBackendError::Other("corrupt asynchronous notification wake port".into())
        })?,
        transaction_open,
        next_sequence: nonnegative_u64(sequence, "listener sequence")?,
        position: nonnegative_u64(position, "listener position")?,
    })
}

pub(super) fn read_listener(
    row: &Row<'_>,
    maximum: Option<usize>,
) -> StorageBackendResult<NotificationListenerRow> {
    let metadata = metadata(row)?;
    let json = row
        .get_ref(7)
        .and_then(|value| value.as_str().map_err(rusqlite::Error::from))
        .map_err(|error| registry_error("read listener channels", &error))?;
    let channels = if let Some(maximum) = maximum {
        // Two brackets plus quotes, a comma and worst-case six-byte escaping of each channel byte.
        if json.len()
            > maximum
                .saturating_mul((MAX_NOTIFICATION_CHANNEL_BYTES - 1) * 6 + 3)
                .saturating_add(2)
        {
            return Err(StorageBackendError::Other(
                "asynchronous notification listener channels exceed their retained limit".into(),
            ));
        }
        let mut decoder = serde_json::Deserializer::from_str(json);
        let channels = Channels {
            maximum,
            allocation: maximum.min(json.len() / 3 + 1),
        }
        .deserialize(&mut decoder)
        .map_err(|error| decode_error(&error))?;
        decoder.end().map_err(|error| decode_error(&error))?;
        channels
    } else {
        serde_json::from_str(json).map_err(|error| decode_error(&error))?
    };
    Ok(NotificationListenerRow {
        owner_id: metadata.key.owner_id,
        session_id: metadata.key.session_id,
        process_id: metadata.process_id,
        wake_port: metadata.wake_port,
        channels,
        transaction_open: metadata.transaction_open,
        next_sequence: metadata.next_sequence,
        position: metadata.position,
    })
}

fn decode_error(error: &serde_json::Error) -> StorageBackendError {
    StorageBackendError::Other(format!(
        "decode asynchronous notification listener channels: {error}"
    ))
}

struct Channels {
    maximum: usize,
    allocation: usize,
}
impl<'de> DeserializeSeed<'de> for Channels {
    type Value = Vec<String>;
    fn deserialize<D: de::Deserializer<'de>>(self, decoder: D) -> Result<Self::Value, D::Error> {
        decoder.deserialize_seq(self)
    }
}
impl<'de> Visitor<'de> for Channels {
    type Value = Vec<String>;
    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a bounded listener channel array")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
        let mut channels = Vec::new();
        channels
            .try_reserve_exact(self.allocation)
            .map_err(de::Error::custom)?;
        while channels.len() < self.maximum {
            let Some(channel) = sequence.next_element::<String>()? else {
                return Ok(channels);
            };
            if channel.len() >= MAX_NOTIFICATION_CHANNEL_BYTES {
                return Err(de::Error::custom("listener channel exceeds its byte limit"));
            }
            channels.push(channel);
        }
        let _: Option<()> = sequence.next_element_seed(ExcessChannel)?;
        Ok(channels)
    }
}
struct ExcessChannel;
impl<'de> DeserializeSeed<'de> for ExcessChannel {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(self, _decoder: D) -> Result<(), D::Error> {
        Err(de::Error::custom(
            "listener channel count exceeds its retained limit",
        ))
    }
}
