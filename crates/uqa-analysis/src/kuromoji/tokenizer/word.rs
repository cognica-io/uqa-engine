//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Japanese lattice phrases and emitted segment morphology have separate lookup paths.

use crate::kuromoji::{DictionaryResult, KuromojiDictionary, UserDictionary};
use crate::morphology::lattice::WordId;

use super::KuromojiOrigin;

pub(super) struct Costs {
    pub left: u16,
    pub right: u16,
    pub word: i32,
}

pub(super) fn costs(
    id: WordId,
    model: &KuromojiDictionary,
    user: Option<&UserDictionary>,
) -> Costs {
    match id {
        WordId::Known(id) | WordId::Unknown(id) => {
            let word = model.word(id).expect("validated lattice word");
            Costs {
                left: word.left_context(),
                right: word.right_context(),
                word: i32::from(word.cost()),
            }
        }
        WordId::User(id) => {
            let phrase = user
                .expect("selected user model")
                .entry(id)
                .expect("matched phrase");
            Costs {
                left: phrase.left_context(),
                right: phrase.right_context(),
                word: phrase.cost(),
            }
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum TokenWord {
    Dictionary(u32, KuromojiOrigin),
    UserSegment(u32),
}

impl TokenWord {
    pub fn origin(self) -> KuromojiOrigin {
        match self {
            Self::Dictionary(_, origin) => origin,
            Self::UserSegment(_) => KuromojiOrigin::User,
        }
    }

    pub fn attributes<'a>(
        self,
        model: &'a KuromojiDictionary,
        user: Option<&'a UserDictionary>,
    ) -> DictionaryResult<[Option<&'a str>; 6]> {
        match self {
            Self::Dictionary(id, _) => {
                let word = model.word(id).expect("validated emitted word");
                Ok([
                    Some(word.part_of_speech()),
                    word.base_form(),
                    word.reading(),
                    word.pronunciation(),
                    word.inflection_type(),
                    word.inflection_form(),
                ])
            }
            Self::UserSegment(id) => {
                let word = user
                    .expect("selected user model")
                    .word(id)
                    .expect("validated emitted segment");
                Ok([
                    Some(word.part_of_speech()?),
                    None,
                    Some(word.reading()?),
                    None,
                    None,
                    None,
                ])
            }
        }
    }
}
