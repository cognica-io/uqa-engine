//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded, canonical identities without transport or random-source ownership.

use std::{fmt, str::FromStr};

/// Invalid subscription identity syntax; diagnostics never retain the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InvalidNotificationIdentity {
    #[error("notification epoch must be a canonical UUID version 4")]
    Epoch,
    #[error("notification request identity must contain 1 to 128 ASCII letters, digits, - or _")]
    RequestId,
}

/// One UUID version 4 identifying a subscription epoch.
///
/// Constructors validate representation only. The delivery adapter must obtain fresh random bytes for each new registration; Core does not own an RNG.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct NotificationEpoch([u8; 16]);

impl NotificationEpoch {
    pub const fn from_bytes(bytes: [u8; 16]) -> Result<Self, InvalidNotificationIdentity> {
        if bytes[6] >> 4 != 4 || bytes[8] >> 6 != 2 {
            return Err(InvalidNotificationIdentity::Epoch);
        }
        Ok(Self(bytes))
    }

    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl FromStr for NotificationEpoch {
    type Err = InvalidNotificationIdentity;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let input = value.as_bytes();
        if input.len() != 36 {
            return Err(InvalidNotificationIdentity::Epoch);
        }
        let mut bytes = [0; 16];
        let mut offset = 0;
        for (index, byte) in bytes.iter_mut().enumerate() {
            if matches!(index, 4 | 6 | 8 | 10) {
                if input[offset] != b'-' {
                    return Err(InvalidNotificationIdentity::Epoch);
                }
                offset += 1;
            }
            let digit = |value| match value {
                b'0'..=b'9' => Ok(value - b'0'),
                b'a'..=b'f' => Ok(value - b'a' + 10),
                _ => Err(InvalidNotificationIdentity::Epoch),
            };
            *byte = digit(input[offset])? << 4 | digit(input[offset + 1])?;
            offset += 2;
        }
        Self::from_bytes(bytes)
    }
}

impl fmt::Display for NotificationEpoch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, byte) in self.0.iter().enumerate() {
            if matches!(index, 4 | 6 | 8 | 10) {
                f.write_str("-")?;
            }
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for NotificationEpoch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// Bounded request metadata, absent from direct embedded subscriptions.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NotificationRequestId(Box<str>);

impl NotificationRequestId {
    pub fn new(value: &str) -> Result<Self, InvalidNotificationIdentity> {
        if value.is_empty()
            || value.len() > 128
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(InvalidNotificationIdentity::RequestId);
        }
        Ok(Self(value.into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for NotificationRequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
