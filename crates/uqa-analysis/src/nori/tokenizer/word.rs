//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A common view of system, unknown, and user morphology without narrowing user costs.

use super::lattice::WordId;
use super::{NoriMorpheme, NoriOrigin};
use crate::nori::{
    DictionaryResult, DictionaryWord, NoriDictionary, POSTag, POSType, UserDictionary, UserEntry,
};

pub(super) enum Word<'a> {
    Dictionary(DictionaryWord<'a>, NoriOrigin),
    User(&'a UserEntry),
}

impl<'a> Word<'a> {
    pub fn resolve(
        id: WordId,
        model: &'a NoriDictionary,
        user: Option<&'a UserDictionary>,
    ) -> Self {
        match id {
            WordId::Known(id) => Self::Dictionary(
                model.word(id).expect("validated known word"),
                NoriOrigin::Known,
            ),
            WordId::Unknown(id) => Self::Dictionary(
                model.word(id).expect("validated unknown word"),
                NoriOrigin::Unknown,
            ),
            WordId::User(id) => Self::User(
                user.expect("user candidate has a model")
                    .entry(id)
                    .expect("validated user word"),
            ),
        }
    }
    pub fn cost(&self) -> i32 {
        match self {
            Self::Dictionary(word, _) => i32::from(word.cost()),
            Self::User(word) => word.cost(),
        }
    }
    pub fn left(&self) -> u16 {
        match self {
            Self::Dictionary(word, _) => word.left_context(),
            Self::User(word) => word.left_context(),
        }
    }
    pub fn right(&self) -> u16 {
        match self {
            Self::Dictionary(word, _) => word.right_context(),
            Self::User(word) => word.right_context(),
        }
    }
    pub fn pos_type(&self) -> POSType {
        match self {
            Self::Dictionary(word, _) => word.pos_type(),
            Self::User(word) => word.pos_type(),
        }
    }
    pub fn left_pos(&self) -> POSTag {
        match self {
            Self::Dictionary(word, _) => word.left_pos(),
            Self::User(word) => word.pos(),
        }
    }
    pub fn right_pos(&self) -> POSTag {
        match self {
            Self::Dictionary(word, _) => word.right_pos(),
            Self::User(word) => word.pos(),
        }
    }
    pub fn origin(&self) -> NoriOrigin {
        match self {
            Self::Dictionary(_, origin) => *origin,
            Self::User(_) => NoriOrigin::User,
        }
    }
    pub fn reading(&self) -> Option<&str> {
        match self {
            Self::Dictionary(word, _) => word.reading(),
            Self::User(_) => None,
        }
    }
    pub fn morphemes(&self, surface: &[u16]) -> DictionaryResult<Option<Vec<NoriMorpheme>>> {
        let mut result = Vec::new();
        match self {
            Self::Dictionary(word, _) => {
                let Some(parts) = word.morphemes() else {
                    return Ok(None);
                };
                result.try_reserve(parts.len())?;
                for part in parts {
                    result.push(NoriMorpheme {
                        surface_utf16: part.surface.encode_utf16().collect(),
                        pos: part.pos,
                    });
                }
            }
            Self::User(word) => {
                let Some(lengths) = word.segment_lengths() else {
                    return Ok(None);
                };
                result.try_reserve(lengths.len())?;
                let mut offset = 0;
                for length in lengths {
                    result.push(NoriMorpheme {
                        surface_utf16: surface[offset..offset + length].to_vec(),
                        pos: word.pos(),
                    });
                    offset += length;
                }
            }
        }
        Ok(Some(result))
    }
}
