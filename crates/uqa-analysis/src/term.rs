//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lossless term identity for Unicode strings and unpaired UTF-16 units.

use std::borrow::Cow;
use std::ops::Range;

use serde::{Deserialize, Serialize, Serializer};

use crate::{AnalysisError, AnalysisResult};

mod allocation;
pub(crate) use allocation::TermBuffer;

#[derive(Clone)]
enum Characters<'a> {
    Unicode(std::str::Chars<'a>),
    UTF16(std::char::DecodeUtf16<std::iter::Copied<std::slice::Iter<'a, u16>>>),
}

impl Iterator for Characters<'_> {
    type Item = Result<char, u16>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Unicode(characters) => characters.next().map(Ok),
            Self::UTF16(characters) => characters
                .next()
                .map(|value| value.map_err(|error| error.unpaired_surrogate())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Representation {
    Unicode(String),
    UTF16(Vec<u16>),
}

/// Canonical term text. Valid UTF-16 is stored as a string; unpaired units remain exact.
///
/// Serialization uses a JSON string for scalar text or an explicit `{"utf16":[...]}` object for unpaired units. Scalar and raw-unit construction give identical identity for the same valid text.
///
/// ```
/// use uqa_analysis::TokenTerm;
/// let term = TokenTerm::from_utf16(vec![0xd83d]);
/// assert!(term.as_str().is_none());
/// assert_eq!(term.utf16().as_ref(), [0xd83d]);
/// assert!(term.into_string().is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TokenTerm(Representation);

impl TokenTerm {
    pub(crate) fn characters(&self) -> impl Iterator<Item = Result<char, u16>> + Clone + '_ {
        match &self.0 {
            Representation::Unicode(text) => Characters::Unicode(text.chars()),
            Representation::UTF16(units) => {
                Characters::UTF16(char::decode_utf16(units.iter().copied()))
            }
        }
    }

    pub fn from_utf16(units: Vec<u16>) -> Self {
        match String::from_utf16(&units) {
            Ok(text) => Self::from(text),
            Err(_) => Self(Representation::UTF16(units)),
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match &self.0 {
            Representation::Unicode(text) => Some(text),
            Representation::UTF16(_) => None,
        }
    }

    pub fn utf16(&self) -> Cow<'_, [u16]> {
        match &self.0 {
            Representation::Unicode(text) => Cow::Owned(text.encode_utf16().collect()),
            Representation::UTF16(units) => Cow::Borrowed(units),
        }
    }

    pub fn into_utf16(self) -> Vec<u16> {
        match self.0 {
            Representation::Unicode(text) => text.encode_utf16().collect(),
            Representation::UTF16(units) => units,
        }
    }

    /// Project to a Unicode string, returning an error instead of replacing unpaired units.
    pub fn into_string(self) -> AnalysisResult<String> {
        match self.0 {
            Representation::Unicode(text) => Ok(text),
            Representation::UTF16(units) => {
                let unit = char::decode_utf16(units)
                    .find_map(Result::err)
                    .expect("non-scalar representation")
                    .unpaired_surrogate();
                Err(AnalysisError::UnpairedTokenSurrogate { unit })
            }
        }
    }

    /// Count each scalar or isolated surrogate as one character.
    pub fn character_count(&self) -> usize {
        match &self.0 {
            Representation::Unicode(text) => text.chars().count(),
            Representation::UTF16(units) => char::decode_utf16(units.iter().copied()).count(),
        }
    }

    pub fn utf16_len(&self) -> usize {
        match &self.0 {
            Representation::Unicode(text) => text.encode_utf16().count(),
            Representation::UTF16(units) => units.len(),
        }
    }

    #[cfg(test)]
    pub(crate) fn map_unicode(&self, transform: impl Fn(&str) -> String) -> Self {
        if let Some(text) = self.as_str() {
            return Self::from(transform(text));
        }
        let mut output = Vec::new();
        let mut text = String::new();
        for character in char::decode_utf16(self.utf16().iter().copied()) {
            match character {
                Ok(character) => text.push(character),
                Err(error) => {
                    output.extend(transform(&text).encode_utf16());
                    text.clear();
                    output.push(error.unpaired_surrogate());
                }
            }
        }
        output.extend(transform(&text).encode_utf16());
        Self::from_utf16(output)
    }

    pub(crate) fn boundaries(&self) -> Vec<usize> {
        match &self.0 {
            Representation::Unicode(text) => text
                .char_indices()
                .map(|(offset, _)| offset)
                .chain(std::iter::once(text.len()))
                .collect(),
            Representation::UTF16(units) => {
                let mut boundaries = vec![0];
                let mut offset = 0;
                for character in char::decode_utf16(units.iter().copied()) {
                    offset += character.map_or(1, char::len_utf16);
                    boundaries.push(offset);
                }
                boundaries
            }
        }
    }

    pub(crate) fn substring(&self, range: Range<usize>) -> Self {
        match &self.0 {
            Representation::Unicode(text) => Self::from(text[range].to_owned()),
            Representation::UTF16(units) => Self::from_utf16(units[range].to_vec()),
        }
    }
}

impl From<String> for TokenTerm {
    fn from(value: String) -> Self {
        Self(Representation::Unicode(value))
    }
}

impl From<&str> for TokenTerm {
    fn from(value: &str) -> Self {
        Self::from(value.to_owned())
    }
}

impl PartialEq<str> for TokenTerm {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == Some(other)
    }
}

impl PartialEq<&str> for TokenTerm {
    fn eq(&self, other: &&str) -> bool {
        self == *other
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawUnits {
    utf16: Vec<u16>,
}

impl Serialize for TokenTerm {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match &self.0 {
            Representation::Unicode(text) => text.serialize(serializer),
            Representation::UTF16(units) => {
                use serde::ser::SerializeStruct;
                let mut value = serializer.serialize_struct("RawUnits", 1)?;
                value.serialize_field("utf16", units)?;
                value.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for TokenTerm {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Value {
            Unicode(String),
            UTF16(RawUnits),
        }
        Ok(match Value::deserialize(deserializer)? {
            Value::Unicode(text) => Self::from(text),
            Value::UTF16(raw) => Self::from_utf16(raw.utf16),
        })
    }
}
