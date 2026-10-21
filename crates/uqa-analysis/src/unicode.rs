//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compiler-independent character properties for the persisted analyzer profile.

mod unicode_17;

use icu_properties::{
    props::{CaseIgnorable, Cased, WhiteSpace},
    CodePointSetData, CodePointSetDataBorrowed,
};

// The pinned ICU4X 2.0 data preserves the Unicode 16 behavior of Rust 1.90.
pub(crate) const VERSION: (u8, u8, u8) = (16, 0, 0);

/// Closed set of executable historical profiles; new descriptors retain the default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Profile {
    #[default]
    Unicode16,
    Unicode17,
}

impl Profile {
    pub(crate) fn from_version(version: Option<(u8, u8, u8)>) -> crate::AnalysisResult<Self> {
        match version {
            None | Some((16, 0, 0)) => Ok(Self::Unicode16),
            Some((17, 0, 0)) => Ok(Self::Unicode17),
            _ => Err(crate::AnalysisError::Descriptor(
                "unsupported Unicode profile",
            )),
        }
    }

    pub(crate) fn version(self) -> (u8, u8, u8) {
        match self {
            Self::Unicode16 => VERSION,
            Self::Unicode17 => (17, 0, 0),
        }
    }

    pub(crate) fn lowercase_override(self, character: char) -> Option<char> {
        match self {
            Self::Unicode16 => None,
            Self::Unicode17 => unicode_17::LOWERCASE
                .binary_search_by_key(&character, |&(upper, _)| upper)
                .ok()
                .map(|index| unicode_17::LOWERCASE[index].1),
        }
    }
}
pub(crate) const CASED: CodePointSetDataBorrowed<'static> = CodePointSetData::new::<Cased>();
pub(crate) const CASE_IGNORABLE: CodePointSetDataBorrowed<'static> =
    CodePointSetData::new::<CaseIgnorable>();

pub(crate) fn is_whitespace(character: char) -> bool {
    // Unicode 16 and 17 have identical White_Space sets (see the retained-profile fixture).
    CodePointSetData::new::<WhiteSpace>().contains(character)
}
