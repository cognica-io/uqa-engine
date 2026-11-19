//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Closed native scalar vocabularies serialize through their existing public implementations.

use serde::{Serialize, Serializer};

#[derive(Clone, Copy)]
pub(in crate::configuration) enum Scalar {
    Null,
    Boolean(bool),
    Unsigned(usize),
    Signed(i32),
    #[cfg(feature = "nori")]
    POSTag(crate::nori::POSTag),
    #[cfg(feature = "nori")]
    DecompoundMode(crate::nori::DecompoundMode),
    #[cfg(feature = "kuromoji")]
    KuromojiMode(crate::kuromoji::KuromojiMode),
    #[cfg(feature = "kuromoji")]
    CompletionMode(crate::kuromoji::CompletionMode),
}

impl From<bool> for Scalar {
    fn from(value: bool) -> Self {
        Self::Boolean(value)
    }
}
impl From<usize> for Scalar {
    fn from(value: usize) -> Self {
        Self::Unsigned(value)
    }
}
impl From<i32> for Scalar {
    fn from(value: i32) -> Self {
        Self::Signed(value)
    }
}

impl Serialize for Scalar {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Null => serializer.serialize_unit(),
            Self::Boolean(value) => value.serialize(serializer),
            Self::Unsigned(value) => value.serialize(serializer),
            Self::Signed(value) => value.serialize(serializer),
            #[cfg(feature = "nori")]
            Self::POSTag(value) => value.serialize(serializer),
            #[cfg(feature = "nori")]
            Self::DecompoundMode(value) => value.serialize(serializer),
            #[cfg(feature = "kuromoji")]
            Self::KuromojiMode(value) => value.serialize(serializer),
            #[cfg(feature = "kuromoji")]
            Self::CompletionMode(value) => value.serialize(serializer),
        }
    }
}
