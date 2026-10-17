//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

// Adapted from ICU4X 2.0.0 (Unicode-3.0); see LICENSE-ICU and PROVENANCE.md.

use super::{
    elements::{in_inclusive_range, CharacterAndClass, COMBINING_CHARACTER_BUFFER_SIZE},
    stream::CollationElements,
};
use icu_properties::props::CanonicalCombiningClass;
use uqa_core::{memory::ProductionSmallVec, ValueRetentionError};

impl<'control, I> CollationElements<'_, 'control, I>
where
    I: Iterator<Item = char>,
{
    pub(super) fn collect_combining(
        &mut self,
        combining_characters: &mut ProductionSmallVec<
            'control,
            [CharacterAndClass; COMBINING_CHARACTER_BUFFER_SIZE],
        >,
    ) -> Result<(), ValueRetentionError> {
        self.control.check()?;
        while !self.is_next_decomposition_starts_with_starter() {
            self.control.check()?;
            #[allow(clippy::unwrap_used)]
            let combining = self.next_internal()?.unwrap().c_and_c;
            let combining_c = combining.character();
            if in_inclusive_range(combining_c, '\u{0340}', '\u{0F81}') {
                match combining_c {
                    '\u{0340}' => {
                        combining_characters.push(CharacterAndClass::new(
                            '\u{0300}',
                            CanonicalCombiningClass::Above,
                        ))?;
                    }
                    '\u{0341}' => {
                        combining_characters.push(CharacterAndClass::new(
                            '\u{0301}',
                            CanonicalCombiningClass::Above,
                        ))?;
                    }
                    '\u{0343}' => {
                        combining_characters.push(CharacterAndClass::new(
                            '\u{0313}',
                            CanonicalCombiningClass::Above,
                        ))?;
                    }
                    '\u{0344}' => {
                        combining_characters.push(CharacterAndClass::new(
                            '\u{0308}',
                            CanonicalCombiningClass::Above,
                        ))?;
                        combining_characters.push(CharacterAndClass::new(
                            '\u{0301}',
                            CanonicalCombiningClass::Above,
                        ))?;
                    }
                    '\u{0F73}' => {
                        combining_characters.push(CharacterAndClass::new(
                            '\u{0F71}',
                            CanonicalCombiningClass::CCC129,
                        ))?;
                        combining_characters.push(CharacterAndClass::new(
                            '\u{0F72}',
                            CanonicalCombiningClass::CCC130,
                        ))?;
                    }
                    '\u{0F75}' => {
                        combining_characters.push(CharacterAndClass::new(
                            '\u{0F71}',
                            CanonicalCombiningClass::CCC129,
                        ))?;
                        combining_characters.push(CharacterAndClass::new(
                            '\u{0F74}',
                            CanonicalCombiningClass::CCC132,
                        ))?;
                    }
                    '\u{0F81}' => {
                        combining_characters.push(CharacterAndClass::new(
                            '\u{0F71}',
                            CanonicalCombiningClass::CCC129,
                        ))?;
                        combining_characters.push(CharacterAndClass::new(
                            '\u{0F80}',
                            CanonicalCombiningClass::CCC130,
                        ))?;
                    }
                    _ => {
                        combining_characters.push(combining)?;
                    }
                }
            } else {
                combining_characters.push(combining)?;
            }
        }
        if combining_characters.len() > 1 {
            for cc in combining_characters.iter_mut() {
                self.control.check()?;
                cc.set_ccc_from_trie_if_not_already_set(self.trie);
            }
        }

        Ok(())
    }
}
