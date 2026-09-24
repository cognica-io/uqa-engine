//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Committed subscription changes share the original publication identity and admission.

use super::{
    hex_digit, invalid, write_number, BudgetedVec, Cursor, NotificationListenerRow,
    StorageReadControl, VersionResult, MAX_NOTIFICATION_CHANNEL_BYTES,
};

#[derive(Clone, Copy)]
pub(super) struct Subscription {
    owner_id: [u8; 16],
    session_id: u64,
    wake_port: u16,
    next_sequence: u64,
    position: u64,
    channels_offset: usize,
    channel_count: u64,
}

impl Subscription {
    pub(super) fn view(self, bytes: &[u8]) -> NotificationSubscriptionView<'_> {
        NotificationSubscriptionView {
            owner_id: self.owner_id,
            session_id: self.session_id,
            wake_port: self.wake_port,
            next_sequence: self.next_sequence,
            position: self.position,
            channel_bytes: &bytes[self.channels_offset..],
            channel_count: self.channel_count,
        }
    }
}

/// An empty channel list removes the listener. Other changes end its SQL transaction and preserve the committed queue cursor captured before publication.
#[derive(Clone, Copy)]
pub struct NotificationSubscriptionView<'a> {
    pub owner_id: [u8; 16],
    pub session_id: u64,
    pub wake_port: u16,
    pub next_sequence: u64,
    pub position: u64,
    channel_bytes: &'a [u8],
    channel_count: u64,
}

impl<'a> NotificationSubscriptionView<'a> {
    pub const fn is_unlisten(&self) -> bool {
        self.channel_count == 0
    }

    /// Reuse the common controlled JSON writer for registries that store a channel array. Iteration borrows the original record; only the encoded JSON buffer is allocated.
    pub fn channels_json(&self, control: &StorageReadControl) -> VersionResult<BudgetedVec<u8>> {
        struct Channels<'a>(NotificationSubscriptionView<'a>);
        impl serde::Serialize for Channels<'_> {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                use serde::ser::SerializeSeq;
                let mut sequence = serializer.serialize_seq(None)?;
                for channel in self.0.channels() {
                    sequence.serialize_element(channel.map_err(serde::ser::Error::custom)?)?;
                }
                sequence.end()
            }
        }
        crate::key_value::record_json::encode(&Channels(*self), control)
    }

    pub fn channels(&self) -> impl Iterator<Item = VersionResult<&'a str>> + 'a {
        let mut cursor = Cursor {
            rest: self.channel_bytes,
        };
        let mut remaining = self.channel_count;
        std::iter::from_fn(move || {
            if remaining == 0 {
                return None;
            }
            remaining -= 1;
            Some(cursor.text(MAX_NOTIFICATION_CHANNEL_BYTES))
        })
    }
}

pub(super) fn encode(
    bytes: &mut BudgetedVec<u8>,
    listener: Option<&NotificationListenerRow>,
    process_id: i32,
    control: &StorageReadControl,
) -> VersionResult<()> {
    let Some(listener) = listener else {
        bytes.extend_from_slice(b"\n0\n")?;
        return Ok(());
    };
    if listener.owner_id == [0; 16]
        || listener.process_id != process_id
        || listener.wake_port == 0
        || listener.transaction_open
        || i64::try_from(listener.next_sequence).is_err()
        || i64::try_from(listener.position).is_err()
    {
        return Err(invalid());
    }
    bytes.extend_from_slice(b"\n1\n")?;
    for byte in listener.owner_id {
        bytes.push(b"0123456789abcdef"[usize::from(byte >> 4)])?;
        bytes.push(b"0123456789abcdef"[usize::from(byte & 15)])?;
    }
    bytes.push(b'\n')?;
    for value in [
        listener.session_id,
        u64::from(listener.wake_port),
        listener.next_sequence,
        listener.position,
        u64::try_from(listener.channels.len()).map_err(|_| invalid())?,
    ] {
        write_number(bytes, value, b'\n')?;
    }
    for channel in &listener.channels {
        control.cancellation().check()?;
        if channel.is_empty() || channel.len() >= MAX_NOTIFICATION_CHANNEL_BYTES {
            return Err(invalid());
        }
        write_number(
            bytes,
            u64::try_from(channel.len()).map_err(|_| invalid())?,
            b':',
        )?;
        bytes.extend_from_slice(channel.as_bytes())?;
    }
    Ok(())
}

pub(super) fn decode(
    cursor: &mut Cursor<'_>,
    record_len: usize,
    control: &StorageReadControl,
) -> VersionResult<Option<Subscription>> {
    if cursor.take(1)? != b"\n" {
        return Err(invalid());
    }
    match cursor.number(b'\n')? {
        0 => return Ok(None),
        1 => {}
        _ => return Err(invalid()),
    }
    let mut owner_id = [0; 16];
    for byte in &mut owner_id {
        let hex = cursor.take(2)?;
        *byte = (hex_digit(hex[0])? << 4) | hex_digit(hex[1])?;
    }
    if cursor.take(1)? != b"\n" || owner_id == [0; 16] {
        return Err(invalid());
    }
    let session_id = cursor.number(b'\n')?;
    let wake_port = u16::try_from(cursor.number(b'\n')?).map_err(|_| invalid())?;
    let next_sequence = cursor.number(b'\n')?;
    let position = cursor.number(b'\n')?;
    let channel_count = cursor.number(b'\n')?;
    if wake_port == 0
        || i64::try_from(next_sequence).is_err()
        || i64::try_from(position).is_err()
        || channel_count > u64::try_from(cursor.rest.len() / 3).map_err(|_| invalid())?
    {
        return Err(invalid());
    }
    let channels_offset = record_len - cursor.rest.len();
    for _ in 0..channel_count {
        control.cancellation().check()?;
        if cursor.text(MAX_NOTIFICATION_CHANNEL_BYTES)?.is_empty() {
            return Err(invalid());
        }
    }
    Ok(Some(Subscription {
        owner_id,
        session_id,
        wake_port,
        next_sequence,
        position,
        channels_offset,
        channel_count,
    }))
}
