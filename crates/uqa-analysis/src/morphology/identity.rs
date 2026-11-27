//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Formatting and parsing of opaque, language-typed dictionary and artifact identities.

use super::error::invalid;

pub(crate) fn parse(text: &str, section: &'static str) -> super::DictionaryResult<[u8; 32]> {
    if text.len() != 64 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid(section, "expected 64 hexadecimal digits"));
    }
    let mut bytes = [0; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16)
            .map_err(|_| invalid(section, "invalid hexadecimal byte"))?;
    }
    Ok(bytes)
}

pub(crate) fn display(
    bytes: &[u8; 32],
    formatter: &mut std::fmt::Formatter<'_>,
) -> std::fmt::Result {
    for byte in bytes {
        write!(formatter, "{byte:02x}")?;
    }
    Ok(())
}

// Keep separate public key types and the FromStr error of each language.
macro_rules! dictionary_digest {
    ($name:ident, $section:literal, $error:ty) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name([u8; 32]);

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                $crate::morphology::identity::display(&self.0, formatter)
            }
        }

        impl std::str::FromStr for $name {
            type Err = $error;
            fn from_str(text: &str) -> Result<Self, Self::Err> {
                Ok(Self($crate::morphology::identity::parse(text, $section)?))
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_str(self)
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                <String as serde::Deserialize>::deserialize(deserializer)?
                    .parse()
                    .map_err(serde::de::Error::custom)
            }
        }
    };
}
pub(crate) use dictionary_digest;
