//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact channel sets, bounded request materialization and resume rejection.

use super::{json, ProtocolError, MAX_NOTIFICATION_WIRE_BYTES};
use serde::{
    de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer, Serialize,
};
use std::{fmt, num::NonZeroUsize};

/// Immutable exact channel set, stored in bytewise order for bounded membership checks. Request order has no notification-order semantics.
#[derive(Clone, PartialEq, Eq)]
pub struct SubscriptionRequest {
    channels: Vec<String>,
}

impl SubscriptionRequest {
    pub fn new(channels: &[&str], max_channels: NonZeroUsize) -> Result<Self, ProtocolError> {
        check_count(channels.len(), max_channels)?;
        if channels.iter().any(|channel| !valid_channel(channel)) {
            return Err(ProtocolError::InvalidChannels);
        }
        // Count escaped output before copying any caller-owned channel strings.
        json::encode(
            &RequestWire {
                protocol_version: 1,
                channels,
            },
            false,
        )?;
        let mut owned = Vec::new();
        owned
            .try_reserve_exact(channels.len())
            .map_err(|_| ProtocolError::Allocation)?;
        for channel in channels {
            let mut value = String::new();
            value
                .try_reserve_exact(channel.len())
                .map_err(|_| ProtocolError::Allocation)?;
            value.push_str(channel);
            owned.push(value);
        }
        Self::from_channels(owned)
    }

    /// Decode a bounded protocol request before registration. A nonempty resume header is rejected even when it contains whitespace.
    pub fn from_json(
        input: &[u8],
        max_channels: NonZeroUsize,
        last_event_id: Option<&[u8]>,
    ) -> Result<Self, ProtocolError> {
        if last_event_id.is_some_and(|value| !value.is_empty()) {
            return Err(ProtocolError::ResumeUnsupported);
        }
        let input = json::validate(input)?;
        let mut deserializer = serde_json::Deserializer::from_str(input);
        let mut failure = None;
        let request = RequestSeed {
            max_channels,
            failure: &mut failure,
        }
        .deserialize(&mut deserializer)
        .map_err(|_| failure.unwrap_or(ProtocolError::InvalidFields))?;
        deserializer.end().map_err(|_| ProtocolError::InvalidJSON)?;
        Self::from_channels(request)
    }

    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        json::encode(
            &RequestWire {
                protocol_version: 1,
                channels: &self.channels,
            },
            true,
        )
    }

    pub fn channels(&self) -> &[String] {
        &self.channels
    }

    pub(super) fn contains(&self, channel: &str) -> bool {
        self.channels
            .binary_search_by(|value| value.as_str().cmp(channel))
            .is_ok()
    }

    fn from_channels(mut channels: Vec<String>) -> Result<Self, ProtocolError> {
        if channels.is_empty() {
            return Err(ProtocolError::InvalidChannels);
        }
        channels.sort_unstable();
        if channels.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(ProtocolError::InvalidChannels);
        }
        Ok(Self { channels })
    }
}

impl fmt::Debug for SubscriptionRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SubscriptionRequest")
            .field("channel_count", &self.channels.len())
            .finish_non_exhaustive()
    }
}

#[derive(Serialize)]
struct RequestWire<T> {
    protocol_version: u8,
    channels: T,
}

fn check_count(count: usize, max: NonZeroUsize) -> Result<(), ProtocolError> {
    if count == 0 {
        return Err(ProtocolError::InvalidChannels);
    }
    if count > max.get() {
        return Err(ProtocolError::ChannelLimit);
    }
    // Every admitted name needs at least three JSON bytes plus a separator or final bracket.
    if count > MAX_NOTIFICATION_WIRE_BYTES / 4 {
        return Err(ProtocolError::ByteLimit);
    }
    Ok(())
}

fn valid_channel(channel: &str) -> bool {
    !channel.is_empty() && channel.len() <= 63 && !channel.contains('\0')
}

#[derive(Deserialize)]
#[serde(field_identifier, rename_all = "snake_case")]
enum Field {
    ProtocolVersion,
    Channels,
}

struct RequestSeed<'a> {
    max_channels: NonZeroUsize,
    failure: &'a mut Option<ProtocolError>,
}

impl<'de> DeserializeSeed<'de> for RequestSeed<'_> {
    type Value = Vec<String>;
    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for RequestSeed<'_> {
    type Value = Vec<String>;
    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a notification request")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut version = None;
        let mut channels = None;
        while let Some(field) = map.next_key::<Field>()? {
            match field {
                Field::ProtocolVersion => {
                    if version.is_some() {
                        return Err(de::Error::duplicate_field("protocol_version"));
                    }
                    let value = map.next_value::<u64>()?;
                    if value != 1 {
                        *self.failure = Some(ProtocolError::UnsupportedVersion);
                        return Err(de::Error::custom("unsupported notification version"));
                    }
                    version = Some(value);
                }
                Field::Channels => {
                    if channels.is_some() {
                        return Err(de::Error::duplicate_field("channels"));
                    }
                    channels = Some(map.next_value_seed(ChannelsSeed {
                        maximum: self.max_channels,
                        failure: self.failure,
                    })?);
                }
            }
        }
        version.ok_or_else(|| de::Error::missing_field("protocol_version"))?;
        channels.ok_or_else(|| de::Error::missing_field("channels"))
    }
}

struct ChannelsSeed<'a> {
    maximum: NonZeroUsize,
    failure: &'a mut Option<ProtocolError>,
}

impl<'de> DeserializeSeed<'de> for ChannelsSeed<'_> {
    type Value = Vec<String>;
    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for ChannelsSeed<'_> {
    type Value = Vec<String>;
    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("bounded notification channels")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut channels = Vec::new();
        while let Some(channel) = seq.next_element::<String>()? {
            let validation = check_count(channels.len() + 1, self.maximum).and_then(|()| {
                if valid_channel(&channel) {
                    Ok(())
                } else {
                    Err(ProtocolError::InvalidChannels)
                }
            });
            if let Err(error) = validation {
                *self.failure = Some(error);
                return Err(de::Error::custom("invalid notification channels"));
            }
            if channels.len() == channels.capacity() {
                let next = channels
                    .capacity()
                    .saturating_mul(2)
                    .max(4)
                    .min(self.maximum.get())
                    .min(MAX_NOTIFICATION_WIRE_BYTES / 4);
                channels
                    .try_reserve_exact(next - channels.len())
                    .map_err(|_| {
                        *self.failure = Some(ProtocolError::Allocation);
                        de::Error::custom("notification allocation failed")
                    })?;
            }
            channels.push(channel);
        }
        Ok(channels)
    }
}
