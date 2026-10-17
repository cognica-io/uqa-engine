//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

// Adapted from ICU4X 2.0.0 (Unicode-3.0); see LICENSE-ICU and PROVENANCE.md.

use super::elements::{
    char_from_u16, char_from_u32, trie_value_has_ccc,
    trie_value_indicates_special_non_starter_decomposition, unwrap_or_gigo,
    CharacterAndClassAndTrieValue, CollationElement, BACKWARD_COMBINING_MARKER, FDFA_MARKER,
    HANGUL_SYLLABLE_MARKER, HIGH_ZEROS_MASK, JAMO_COUNT, LOW_ZEROS_MASK, NON_ROUND_TRIP_MARKER,
    PENDING_CE_BUFFER_SIZE, SINGLE_REPLACEMENT_CHARACTER_CHAR, SINGLE_REPLACEMENT_CHARACTER_U16,
    UPCOMING_CHARACTER_BUFFER_SIZE,
};
use icu_collator::provider::CollationData;
use icu_collections::codepointtrie::CodePointTrie;
use icu_normalizer::provider::{DecompositionData, DecompositionTables};
use icu_properties::props::CanonicalCombiningClass;
use uqa_core::{
    memory::{ProductionControl, ProductionSmallVec},
    ValueRetentionError,
};
use zerovec::{ule::AsULE, ZeroSlice};

pub(crate) struct CollationElements<'data, 'control, I>
where
    I: Iterator<Item = char>,
{
    pub(super) iter: I,
    pub(super) control: ProductionControl<'control>,
    pub(super) pending: ProductionSmallVec<'control, [CollationElement; PENDING_CE_BUFFER_SIZE]>,
    pub(super) pending_pos: usize,
    pub(super) prefix: [char; 2],
    pub(super) upcoming: ProductionSmallVec<
        'control,
        [CharacterAndClassAndTrieValue; UPCOMING_CHARACTER_BUFFER_SIZE],
    >,
    pub(super) root: &'data CollationData<'data>,
    pub(super) jamo: &'data [<u32 as AsULE>::ULE; JAMO_COUNT],
    pub(super) diacritics: &'data ZeroSlice<u16>,
    pub(super) trie: &'data CodePointTrie<'data, u32>,
    pub(super) scalars16: &'data ZeroSlice<u16>,
    pub(super) scalars32: &'data ZeroSlice<char>,
    pub(super) upcoming_decomposed: bool,
    #[cfg(debug_assertions)]
    pub(super) iter_exhausted: bool,
    #[cfg(debug_assertions)]
    pub(super) initialized: bool,
}

impl<'data, 'control, I> CollationElements<'data, 'control, I>
where
    I: Iterator<Item = char>,
{
    pub fn new(
        delegate: I,
        control: ProductionControl<'control>,
        root: &'data CollationData,
        jamo: &'data [<u32 as AsULE>::ULE; JAMO_COUNT],
        diacritics: &'data ZeroSlice<u16>,
        decompositions: &'data DecompositionData,
        tables: &'data DecompositionTables,
    ) -> Self {
        CollationElements::<I> {
            iter: delegate,
            control,
            pending: ProductionSmallVec::new(control),
            pending_pos: 0,
            prefix: ['\u{FFFF}'; 2],
            upcoming: ProductionSmallVec::new(control),
            root,
            jamo,
            diacritics,
            trie: &decompositions.trie,
            scalars16: &tables.scalars16,
            scalars32: &tables.scalars24,
            upcoming_decomposed: false,
            #[cfg(debug_assertions)]
            iter_exhausted: false,
            #[cfg(debug_assertions)]
            initialized: false,
        }
    }

    pub fn init(&mut self) -> Result<(), ValueRetentionError> {
        self.control.check()?;
        #[cfg(debug_assertions)]
        {
            debug_assert!(!self.initialized);
            self.initialized = true;
        }

        if let Some(last) = self.upcoming.last() {
            if last.decomposition_starts_with_non_starter() {
                loop {
                    self.control.check()?;
                    if let Some(ch) = self.iter_next()? {
                        let starter = !ch.decomposition_starts_with_non_starter();
                        self.upcoming.push(ch)?;
                        if starter {
                            break;
                        }
                    } else {
                        #[cfg(debug_assertions)]
                        {
                            self.iter_exhausted = true;
                        }
                        break;
                    }
                }
            }
        }

        let mut starts_with_starter = false;
        if let Some(first) = self.upcoming.first() {
            if !first.decomposition_starts_with_non_starter() {
                starts_with_starter = true;
            }
        }
        if !starts_with_starter {
            self.upcoming.insert(
                0,
                CharacterAndClassAndTrieValue::new_with_non_decomposing_starter('\u{FFFF}'),
            )?; // Make sure the process always begins with a starter
            let _ = self.next()?; // Remove the placeholder starter
        }

        Ok(())
    }

    pub(super) fn iter_next(
        &mut self,
    ) -> Result<Option<CharacterAndClassAndTrieValue>, ValueRetentionError> {
        self.control.check()?;
        let Some(c) = self.iter.next() else {
            return Ok(None);
        };
        let trie_val = self.trie.get(c);
        Ok(Some(CharacterAndClassAndTrieValue::new_with_trie_val(
            c, trie_val,
        )))
    }

    pub(super) fn next_internal(
        &mut self,
    ) -> Result<Option<CharacterAndClassAndTrieValue>, ValueRetentionError> {
        self.control.check()?;
        if self.upcoming.is_empty() {
            return Ok(None);
        }
        let ret = self.upcoming.remove(0)?;
        if self.upcoming.is_empty() {
            if let Some(c) = self.iter_next()? {
                self.upcoming.push(c)?;
            } else {
                #[cfg(debug_assertions)]
                {
                    self.iter_exhausted = true;
                }
            }
        }
        Ok(Some(ret))
    }

    pub(super) fn maybe_gather_combining(&mut self) -> Result<(), ValueRetentionError> {
        self.control.check()?;
        if self.upcoming.len() != 1 {
            return Ok(());
        }

        if !self.upcoming[0].decomposition_starts_with_non_starter() {
            return Ok(());
        }
        let first = self.upcoming.remove(0)?;
        self.push_decomposed_combining(first)?;
        loop {
            self.control.check()?;
            if let Some(ch) = self.iter_next()? {
                if ch.decomposition_starts_with_non_starter() {
                    self.push_decomposed_combining(ch)?;
                } else {
                    self.upcoming.push(ch)?;
                    break;
                }
            } else {
                #[cfg(debug_assertions)]
                {
                    self.iter_exhausted = true;
                }
                break;
            }
        }

        Ok(())
    }

    pub(super) fn ensure_upcoming_decomposed(&mut self) -> Result<(), ValueRetentionError> {
        self.control.check()?;
        if self.upcoming_decomposed {
            return Ok(());
        }
        self.upcoming_decomposed = true;
        let without_trailing_starter = if let Some((last, head)) = self.upcoming.split_last() {
            if last.decomposition_starts_with_non_starter() {
                &self.upcoming[..]
            } else if head.is_empty() {
                return Ok(());
            } else {
                head
            }
        } else {
            #[cfg(debug_assertions)]
            debug_assert!(self.iter_exhausted);
            return Ok(());
        };

        let mut normalized = true;
        for c in without_trailing_starter {
            self.control.check()?;
            if (c.trie_val
                & !(BACKWARD_COMBINING_MARKER | NON_ROUND_TRIP_MARKER | HANGUL_SYLLABLE_MARKER))
                != 0
            {
                normalized = false;
                break;
            }
        }
        if normalized {
            return Ok(());
        }

        let unnormalized =
            core::mem::replace(&mut self.upcoming, ProductionSmallVec::new(self.control));
        let last_index = unnormalized.len() - 1;

        {
            debug_assert!(!unnormalized[0].decomposition_starts_with_non_starter());
        }
        for (i, c) in unnormalized.iter().copied().enumerate() {
            self.control.check()?;
            if c.decomposition_starts_with_non_starter() {
                self.push_decomposed_combining(c)?;
            } else if i == last_index {
                self.upcoming.push(c)?;
                return Ok(());
            } else {
                self.push_decomposed_starter(c)?;
            }
        }
        #[cfg(debug_assertions)]
        debug_assert!(self.iter_exhausted);

        Ok(())
    }

    pub(super) fn push_decomposed_combining(
        &mut self,
        c: CharacterAndClassAndTrieValue,
    ) -> Result<(), ValueRetentionError> {
        self.control.check()?;
        if !trie_value_indicates_special_non_starter_decomposition(c.trie_val) {
            debug_assert!(trie_value_has_ccc(c.trie_val));
            self.upcoming.push(c)?;
            return Ok(());
        }

        match c.character() {
            '\u{0340}' => {
                self.upcoming
                    .push(CharacterAndClassAndTrieValue::new_with_non_zero_ccc(
                        '\u{0300}',
                        CanonicalCombiningClass::Above,
                    ))?;
            }
            '\u{0341}' => {
                self.upcoming
                    .push(CharacterAndClassAndTrieValue::new_with_non_zero_ccc(
                        '\u{0301}',
                        CanonicalCombiningClass::Above,
                    ))?;
            }
            '\u{0343}' => {
                self.upcoming
                    .push(CharacterAndClassAndTrieValue::new_with_non_zero_ccc(
                        '\u{0313}',
                        CanonicalCombiningClass::Above,
                    ))?;
            }
            '\u{0344}' => {
                self.upcoming
                    .push(CharacterAndClassAndTrieValue::new_with_non_zero_ccc(
                        '\u{0308}',
                        CanonicalCombiningClass::Above,
                    ))?;
                self.upcoming
                    .push(CharacterAndClassAndTrieValue::new_with_non_zero_ccc(
                        '\u{0301}',
                        CanonicalCombiningClass::Above,
                    ))?;
            }
            '\u{0F73}' => {
                self.upcoming
                    .push(CharacterAndClassAndTrieValue::new_with_non_zero_ccc(
                        '\u{0F71}',
                        CanonicalCombiningClass::CCC129,
                    ))?;
                self.upcoming
                    .push(CharacterAndClassAndTrieValue::new_with_non_zero_ccc(
                        '\u{0F72}',
                        CanonicalCombiningClass::CCC130,
                    ))?;
            }
            '\u{0F75}' => {
                self.upcoming
                    .push(CharacterAndClassAndTrieValue::new_with_non_zero_ccc(
                        '\u{0F71}',
                        CanonicalCombiningClass::CCC129,
                    ))?;
                self.upcoming
                    .push(CharacterAndClassAndTrieValue::new_with_non_zero_ccc(
                        '\u{0F74}',
                        CanonicalCombiningClass::CCC132,
                    ))?;
            }
            '\u{0F81}' => {
                self.upcoming
                    .push(CharacterAndClassAndTrieValue::new_with_non_zero_ccc(
                        '\u{0F71}',
                        CanonicalCombiningClass::CCC129,
                    ))?;
                self.upcoming
                    .push(CharacterAndClassAndTrieValue::new_with_non_zero_ccc(
                        '\u{0F80}',
                        CanonicalCombiningClass::CCC130,
                    ))?;
            }
            _ => {
                debug_assert!(false);
            }
        }

        Ok(())
    }

    pub(super) fn push_decomposed_starter(
        &mut self,
        c: CharacterAndClassAndTrieValue,
    ) -> Result<(), ValueRetentionError> {
        self.control.check()?;

        let decomposition = c.trie_val;
        if (decomposition & !(BACKWARD_COMBINING_MARKER | NON_ROUND_TRIP_MARKER))
            <= HANGUL_SYLLABLE_MARKER
        {
            self.upcoming.push(
                CharacterAndClassAndTrieValue::new_with_non_decomposing_starter(c.character()),
            )?;
        } else {
            let high_zeros = (decomposition & HIGH_ZEROS_MASK) == 0;
            let low_zeros = (decomposition & LOW_ZEROS_MASK) == 0;
            if !high_zeros && !low_zeros {
                let starter = char_from_u32(decomposition & 0x7FFF);
                let low_c = char_from_u32((decomposition >> 15) & 0x7FFF);
                self.upcoming.push(
                    CharacterAndClassAndTrieValue::new_with_non_decomposing_starter(starter),
                )?;
                let trie_value = self.trie.get(low_c);
                self.upcoming.push(
                    CharacterAndClassAndTrieValue::new_with_non_special_decomposition_trie_val(
                        low_c, trie_value,
                    ),
                )?;
            } else if high_zeros {
                let singleton = decomposition as u16;
                debug_assert_ne!(
                    singleton, FDFA_MARKER,
                    "How come U+FDFA NFKD marker seen in NFD?"
                );
                if (singleton & 0xFF00) == 0xD800 {
                    self.upcoming.push(c)?;
                    #[cfg(debug_assertions)]
                    debug_assert!(self.iter_exhausted);
                } else {
                    self.upcoming.push(
                        CharacterAndClassAndTrieValue::new_with_non_decomposing_starter(
                            char_from_u16(singleton),
                        ),
                    )?;
                }
            } else {
                debug_assert!(low_zeros);
                let offset = (((decomposition & !(0b11 << 30)) >> 16) as usize) - 1;
                let len_bits = decomposition & 0b1111;
                if offset < self.scalars16.len() {
                    let len = (len_bits + 2) as usize;
                    for u in unwrap_or_gigo(
                        self.scalars16.get_subslice(offset..offset + len),
                        SINGLE_REPLACEMENT_CHARACTER_U16, // single instead of empty for consistency with the other code path
                    )
                    .iter()
                    {
                        self.control.check()?;
                        let ch = char_from_u16(u);
                        let trie_value = self.trie.get(ch);
                        self.upcoming
                            .push(CharacterAndClassAndTrieValue::new_with_non_special_decomposition_trie_val(ch, trie_value))?;
                    }
                } else {
                    let len = (len_bits + 1) as usize;
                    let offset32 = offset - self.scalars16.len();
                    for ch in unwrap_or_gigo(
                        self.scalars32.get_subslice(offset32..offset32 + len),
                        SINGLE_REPLACEMENT_CHARACTER_CHAR, // single instead of empty for consistency with the other code path
                    )
                    .iter()
                    {
                        self.control.check()?;
                        let trie_value = self.trie.get(ch);
                        self.upcoming
                            .push(CharacterAndClassAndTrieValue::new_with_non_special_decomposition_trie_val(ch, trie_value))?;
                    }
                }
            }
        }
        Ok(())
    }

    pub(super) fn push_decomposed_and_gather_combining(
        &mut self,
        c: CharacterAndClassAndTrieValue,
    ) -> Result<(), ValueRetentionError> {
        self.control.check()?;
        self.push_decomposed_starter(c)?;
        loop {
            self.control.check()?;
            if let Some(ch) = self.iter_next()? {
                if ch.decomposition_starts_with_non_starter() {
                    self.push_decomposed_combining(ch)?;
                } else {
                    self.upcoming.push(ch)?;
                    return Ok(());
                }
            } else {
                #[cfg(debug_assertions)]
                {
                    self.iter_exhausted = true;
                }

                return Ok(());
            }
        }
    }

    pub(super) fn look_ahead(
        &mut self,
        pos: usize,
    ) -> Result<Option<CharacterAndClassAndTrieValue>, ValueRetentionError> {
        self.control.check()?;
        debug_assert!(self.upcoming_decomposed);
        Ok(if pos + 1 == self.upcoming.len() {
            let c = self.upcoming.remove(pos)?;
            self.push_decomposed_and_gather_combining(c)?;
            Some(self.upcoming[pos])
        } else if pos == self.upcoming.len() {
            if let Some(c) = self.iter_next()? {
                debug_assert!(
                    false,
                    "The `upcoming` queue should be empty when iteration `pos` at the end"
                );
                self.push_decomposed_and_gather_combining(c)?;
                Some(self.upcoming[pos])
            } else {
                #[cfg(debug_assertions)]
                {
                    self.iter_exhausted = true;
                }
                None
            }
        } else {
            Some(self.upcoming[pos])
        })
    }

    pub(super) fn is_next_decomposition_starts_with_starter(&self) -> bool {
        if let Some(c_c_tv) = self.upcoming.first() {
            !c_c_tv.decomposition_starts_with_non_starter()
        } else {
            true
        }
    }

    pub(super) fn prepend_nonstarter_suffix(
        &mut self,
        c: CharacterAndClassAndTrieValue,
    ) -> Result<(), ValueRetentionError> {
        self.control.check()?;
        self.upcoming.insert(0, c)
    }

    pub(super) fn prefix_push(&mut self, c: char) {
        self.prefix[1] = self.prefix[0];
        self.prefix[0] = c;
    }

    pub(super) fn mark_prefix_unmatchable(&mut self) {
        self.prefix[0] = '\u{FFFF}';
    }
}
