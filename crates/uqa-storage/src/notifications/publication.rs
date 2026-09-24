//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admitted immutable publication records with borrowed, bounded recovery decoding.

use std::sync::Arc;

use sha2::{Digest, Sha256};
use uqa_core::memory::BudgetedVec;

use super::{
    end_position, PendingNotification, MAX_NOTIFICATION_CHANNEL_BYTES,
    MAX_NOTIFICATION_PAYLOAD_BYTES,
};
use crate::{
    mvcc::{VersionError, VersionResult},
    read_control::StorageReadControl,
};

const MAGIC: &[u8] = b"UQA notification publication 1\n";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotificationPublicationHeader {
    pub registry_id: [u8; 16],
    pub first_sequence: u64,
    pub next_sequence: u64,
    pub first_position: u64,
    pub end_position: u64,
    pub process_id: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotificationMessageRef<'a> {
    pub channel: &'a str,
    pub payload: &'a str,
}

/// One immutable record shares its original admission across clones. Length-framed UTF-8 fields can also occupy native metadata TEXT without escaping or allocating decoded message strings.
#[derive(Clone)]
pub struct NotificationPublication {
    bytes: Arc<BudgetedVec<u8>>,
    header: NotificationPublicationHeader,
    messages_offset: usize,
    fingerprint: [u8; 32],
}

impl NotificationPublication {
    pub fn encode(
        registry_id: [u8; 16],
        first_sequence: u64,
        first_position: u64,
        process_id: i32,
        messages: &[PendingNotification],
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        control.cancellation().check()?;
        let count = u64::try_from(messages.len()).map_err(|_| invalid())?;
        validate_header(
            registry_id,
            first_sequence,
            first_position,
            process_id,
            count,
        )?;
        let mut bytes = BudgetedVec::new(control.memory());
        bytes.extend_from_slice(MAGIC)?;
        for byte in registry_id {
            bytes.push(b"0123456789abcdef"[usize::from(byte >> 4)])?;
            bytes.push(b"0123456789abcdef"[usize::from(byte & 15)])?;
        }
        bytes.push(b'\n')?;
        for value in [
            first_sequence,
            first_position,
            u64::try_from(process_id).map_err(|_| invalid())?,
            count,
        ] {
            write_number(&mut bytes, value, b'\n')?;
        }
        for message in messages {
            control.cancellation().check()?;
            if message.channel.is_empty()
                || message.channel.len() >= MAX_NOTIFICATION_CHANNEL_BYTES
                || message.payload.len() >= MAX_NOTIFICATION_PAYLOAD_BYTES
            {
                return Err(invalid());
            }
            for field in [&message.channel, &message.payload] {
                write_number(
                    &mut bytes,
                    u64::try_from(field.len()).map_err(|_| invalid())?,
                    b':',
                )?;
                bytes.extend_from_slice(field.as_bytes())?;
            }
        }
        let view = NotificationPublicationView::decode(&bytes, control)?;
        let header = view.header;
        let messages_offset = view.messages_offset;
        let fingerprint = view.fingerprint();
        Ok(Self {
            bytes: Arc::new(bytes),
            header,
            messages_offset,
            fingerprint,
        })
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Retain the immutable encoded record and its original memory admission.
    pub fn shared_bytes(&self) -> Arc<BudgetedVec<u8>> {
        Arc::clone(&self.bytes)
    }

    pub const fn header(&self) -> NotificationPublicationHeader {
        self.header
    }

    pub const fn fingerprint(&self) -> [u8; 32] {
        self.fingerprint
    }

    pub fn view(&self) -> NotificationPublicationView<'_> {
        NotificationPublicationView {
            bytes: self.bytes(),
            header: self.header,
            messages_offset: self.messages_offset,
        }
    }
}

/// A validated view borrows its entire record. Recovery can visit one message at a time without materializing a second payload list.
#[derive(Clone, Copy)]
pub struct NotificationPublicationView<'a> {
    bytes: &'a [u8],
    header: NotificationPublicationHeader,
    messages_offset: usize,
}

impl<'a> NotificationPublicationView<'a> {
    pub fn decode(bytes: &'a [u8], control: &StorageReadControl) -> VersionResult<Self> {
        control.cancellation().check()?;
        let mut cursor = Cursor {
            rest: bytes.strip_prefix(MAGIC).ok_or_else(invalid)?,
        };
        let mut registry_id = [0; 16];
        for byte in &mut registry_id {
            let hex = cursor.take(2)?;
            *byte = (hex_digit(hex[0])? << 4) | hex_digit(hex[1])?;
        }
        if cursor.take(1)? != b"\n" {
            return Err(invalid());
        }
        let first_sequence = cursor.number(b'\n')?;
        let first_position = cursor.number(b'\n')?;
        let process_id = i32::try_from(cursor.number(b'\n')?).map_err(|_| invalid())?;
        let count = cursor.number(b'\n')?;
        let next_sequence = validate_header(
            registry_id,
            first_sequence,
            first_position,
            process_id,
            count,
        )?;
        if count > u64::try_from(cursor.rest.len() / 5).map_err(|_| invalid())? {
            return Err(invalid());
        }
        let messages_offset = bytes.len() - cursor.rest.len();
        let mut position = first_position;
        for _ in 0..count {
            control.cancellation().check()?;
            let message = cursor.message()?;
            position = end_position(position, message.channel.len(), message.payload.len());
            if i64::try_from(position).is_err() {
                return Err(invalid());
            }
        }
        if !cursor.rest.is_empty() {
            return Err(invalid());
        }
        Ok(Self {
            bytes,
            header: NotificationPublicationHeader {
                registry_id,
                first_sequence,
                next_sequence,
                first_position,
                end_position: position,
                process_id,
            },
            messages_offset,
        })
    }

    pub const fn header(&self) -> NotificationPublicationHeader {
        self.header
    }

    pub fn fingerprint(&self) -> [u8; 32] {
        Sha256::digest(self.bytes).into()
    }

    pub fn messages(&self) -> impl Iterator<Item = VersionResult<NotificationMessageRef<'a>>> + 'a {
        let mut cursor = Cursor {
            rest: &self.bytes[self.messages_offset..],
        };
        let mut count = self.header.next_sequence - self.header.first_sequence;
        std::iter::from_fn(move || {
            if count == 0 {
                return None;
            }
            count -= 1;
            Some(cursor.message())
        })
    }
}

fn validate_header(
    registry: [u8; 16],
    sequence: u64,
    position: u64,
    process_id: i32,
    count: u64,
) -> VersionResult<u64> {
    let next_sequence = sequence.checked_add(count).ok_or_else(invalid)?;
    if registry == [0; 16]
        || process_id <= 0
        || count == 0
        || i64::try_from(next_sequence).is_err()
        || i64::try_from(position).is_err()
    {
        return Err(invalid());
    }
    Ok(next_sequence)
}

fn write_number(output: &mut BudgetedVec<u8>, mut number: u64, separator: u8) -> VersionResult<()> {
    let mut digits = [0; 20];
    let mut start = digits.len();
    loop {
        start -= 1;
        digits[start] = b'0' + u8::try_from(number % 10).map_err(|_| invalid())?;
        number /= 10;
        if number == 0 {
            break;
        }
    }
    output.extend_from_slice(&digits[start..])?;
    output.push(separator)?;
    Ok(())
}

struct Cursor<'a> {
    rest: &'a [u8],
}

impl<'a> Cursor<'a> {
    fn take(&mut self, length: usize) -> VersionResult<&'a [u8]> {
        let (value, rest) = self.rest.split_at_checked(length).ok_or_else(invalid)?;
        self.rest = rest;
        Ok(value)
    }

    fn number(&mut self, separator: u8) -> VersionResult<u64> {
        let mut number = 0u64;
        for length in 0..=20 {
            let byte = *self.rest.get(length).ok_or_else(invalid)?;
            if byte == separator && length != 0 {
                self.take(length + 1)?;
                return Ok(number);
            }
            if !byte.is_ascii_digit() || (length > 0 && self.rest[0] == b'0') {
                return Err(invalid());
            }
            number = number
                .checked_mul(10)
                .and_then(|value| value.checked_add(u64::from(byte - b'0')))
                .ok_or_else(invalid)?;
        }
        Err(invalid())
    }

    fn text(&mut self, maximum: usize) -> VersionResult<&'a str> {
        let length = usize::try_from(self.number(b':')?).map_err(|_| invalid())?;
        if length >= maximum {
            return Err(invalid());
        }
        std::str::from_utf8(self.take(length)?).map_err(|_| invalid())
    }

    fn message(&mut self) -> VersionResult<NotificationMessageRef<'a>> {
        let channel = self.text(MAX_NOTIFICATION_CHANNEL_BYTES)?;
        if channel.is_empty() {
            return Err(invalid());
        }
        Ok(NotificationMessageRef {
            channel,
            payload: self.text(MAX_NOTIFICATION_PAYLOAD_BYTES)?,
        })
    }
}

fn hex_digit(byte: u8) -> VersionResult<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(invalid()),
    }
}

fn invalid() -> VersionError {
    VersionError::InvalidEncoding("invalid notification publication record")
}

#[cfg(test)]
mod tests;
