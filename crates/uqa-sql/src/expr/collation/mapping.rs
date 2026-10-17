//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

// Adapted from ICU4X 2.0.0 (Unicode-3.0); see LICENSE-ICU and PROVENANCE.md.

use super::{
    data::CollationDataExt,
    elements::{
        ccc_from_trie_value, char_from_u16, char_from_u32, CharacterAndClass,
        CharacterAndClassAndTrieValue, CollationElement, CollationElement32, Tag,
        BACKWARD_COMBINING_MARKER, COMBINING_CHARACTER_BUFFER_SIZE, COMBINING_DIACRITICS_BASE,
        EMPTY_CHAR, EMPTY_U16, FALLBACK_CE32, FDFA_MARKER, FFFD_CE, HANGUL_L_BASE, HANGUL_N_COUNT,
        HANGUL_S_BASE, HANGUL_S_COUNT, HANGUL_T_BASE, HANGUL_T_COUNT, HANGUL_V_BASE,
        HIGH_ZEROS_MASK, LOW_ZEROS_MASK, NON_ROUND_TRIP_MARKER, NO_CE, PENDING_REMOVALS_SIZE,
    },
    stream::CollationElements,
};
use core::char::REPLACEMENT_CHARACTER;
use icu_collator::provider::CollationData;
use icu_collections::char16trie::TrieResult;
use icu_properties::props::CanonicalCombiningClass;
use uqa_core::{memory::ProductionSmallVec, ValueRetentionError};
use zerovec::ZeroSlice;

impl<'control, I> CollationElements<'_, 'control, I>
where
    I: Iterator<Item = char>,
{
    // Keep the audited upstream decomposition/contraction branch structure together.
    #[allow(clippy::too_many_lines)]
    pub fn next(&mut self) -> Result<CollationElement, ValueRetentionError> {
        self.control.check()?;
        #[cfg(debug_assertions)]
        debug_assert!(self.initialized);
        debug_assert!(self.is_next_decomposition_starts_with_starter());
        if let Some(&ret) = self.pending.get(self.pending_pos) {
            self.pending_pos += 1;
            if self.pending_pos == self.pending.len() {
                self.pending.clear();
                self.pending_pos = 0;
            }
            return Ok(ret);
        }
        debug_assert_eq!(self.pending_pos, 0);
        Ok(if let Some(c_c_tv) = self.next_internal()? {
            let mut c = c_c_tv.character();
            let mut ce32;
            let mut data: &CollationData = self.root;
            let mut combining_characters: ProductionSmallVec<
                'control,
                [CharacterAndClass; COMBINING_CHARACTER_BUFFER_SIZE],
            > = ProductionSmallVec::new(self.control);

            let hangul_offset = u32::from(c).wrapping_sub(HANGUL_S_BASE); // SIndex in the spec
            if hangul_offset >= HANGUL_S_COUNT {
                let decomposition = c_c_tv.trie_val;
                if (decomposition & !(BACKWARD_COMBINING_MARKER | NON_ROUND_TRIP_MARKER)) == 0 {
                    let jamo_index = (c as usize).wrapping_sub(HANGUL_L_BASE as usize);

                    if jamo_index >= self.jamo.len() {
                        ce32 = data.ce32_for_char(c);
                        if ce32 == FALLBACK_CE32 {
                            data = self.root;
                            ce32 = data.ce32_for_char(c);
                        }
                    } else {
                        data = self.root;
                        ce32 = CollationElement32::new_from_ule(self.jamo[jamo_index]);
                    }
                    if self.is_next_decomposition_starts_with_starter() {
                        if let Some(ce) = ce32.to_ce_simple_or_long_primary() {
                            self.prefix_push(c);
                            return Ok(ce);
                        } else if ce32.tag() == Tag::Contraction
                            && ce32.every_suffix_starts_with_combining()
                        {
                            let default = data.get_default(ce32.index());
                            if let Some(ce) = default.to_ce_simple_or_long_primary() {
                                self.prefix_push(c);
                                return Ok(ce);
                            }
                        }
                    }
                } else {
                    let high_zeros = (decomposition & HIGH_ZEROS_MASK) == 0;
                    let low_zeros = (decomposition & LOW_ZEROS_MASK) == 0;
                    if !high_zeros && !low_zeros {
                        c = char_from_u32(decomposition & 0x7FFF);
                        ce32 = data.ce32_for_char(c);
                        if ce32 == FALLBACK_CE32 {
                            data = self.root;
                            ce32 = data.ce32_for_char(c);
                        }
                        let combining = char_from_u32((decomposition >> 15) & 0x7FFF);
                        if self.is_next_decomposition_starts_with_starter() {
                            let diacritic_index =
                                (combining as usize).wrapping_sub(COMBINING_DIACRITICS_BASE);
                            if let Some(secondary) = self.diacritics.get(diacritic_index) {
                                debug_assert_ne!(combining, '\u{0344}', "Should never have COMBINING GREEK DIALYTIKA TONOS here, since it should have decomposed further.");
                                if let Some(ce) = ce32.to_ce_simple_or_long_primary() {
                                    let ce_for_combining =
                                        CollationElement::new_from_secondary(secondary);
                                    self.pending.push(ce_for_combining)?;
                                    self.mark_prefix_unmatchable();
                                    return Ok(ce);
                                }
                                if ce32.tag() == Tag::Contraction
                                    && ce32.every_suffix_starts_with_combining()
                                {
                                    let (default, mut trie) =
                                        data.get_default_and_trie(ce32.index());
                                    match trie.next(combining) {
                                        TrieResult::NoMatch | TrieResult::NoValue => {
                                            if let Some(ce) = default.to_ce_simple_or_long_primary()
                                            {
                                                let ce_for_combining =
                                                    CollationElement::new_from_secondary(secondary);
                                                self.pending.push(ce_for_combining)?;
                                                self.mark_prefix_unmatchable();
                                                return Ok(ce);
                                            }
                                        }
                                        TrieResult::Intermediate(trie_ce32) => {
                                            if !ce32.at_least_one_suffix_contains_starter() {
                                                if let Some(ce) =
                                                    CollationElement32::new(trie_ce32 as u32)
                                                        .to_ce_simple_or_long_primary()
                                                {
                                                    self.mark_prefix_unmatchable();
                                                    return Ok(ce);
                                                }
                                            }
                                        }
                                        TrieResult::FinalValue(trie_ce32) => {
                                            if let Some(ce) =
                                                CollationElement32::new(trie_ce32 as u32)
                                                    .to_ce_simple_or_long_primary()
                                            {
                                                self.mark_prefix_unmatchable();
                                                return Ok(ce);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        combining_characters
                            .push(CharacterAndClass::new_with_placeholder(combining))?;
                    } else if high_zeros {
                        let singleton = decomposition as u16;
                        debug_assert_ne!(
                            singleton, FDFA_MARKER,
                            "How come U+FDFA NFKD marker seen in NFD?"
                        );
                        c = char_from_u16(singleton);
                        ce32 = data.ce32_for_char(c);
                        if ce32 == FALLBACK_CE32 {
                            data = self.root;
                            ce32 = data.ce32_for_char(c);
                        }
                        if self.is_next_decomposition_starts_with_starter() {
                            if let Some(ce) = ce32.to_ce_simple_or_long_primary() {
                                self.prefix_push(c);
                                return Ok(ce);
                            }
                        }
                    } else {
                        debug_assert!(low_zeros);
                        let offset = (((decomposition & !(0b11 << 30)) >> 16) as usize) - 1;
                        let len_bits = decomposition & 0b1111;
                        let only_non_starters_in_trail = (decomposition & 0b10000) != 0;
                        if offset < self.scalars16.len() {
                            let len = (len_bits + 2) as usize;
                            let (starter, tail) = self
                                .scalars16
                                .get_subslice(offset..offset + len)
                                .and_then(ZeroSlice::split_first)
                                .map_or_else(
                                    || {
                                        debug_assert!(false);
                                        (REPLACEMENT_CHARACTER, EMPTY_U16)
                                    },
                                    |(first, tail)| (char_from_u16(first), tail),
                                );
                            c = starter;
                            if only_non_starters_in_trail {
                                for u in tail.iter() {
                                    self.control.check()?;
                                    let char_from_u = char_from_u16(u);
                                    let trie_value = self.trie.get(char_from_u);
                                    let ccc = ccc_from_trie_value(trie_value);
                                    combining_characters
                                        .push(CharacterAndClass::new(char_from_u, ccc))?;
                                }
                            } else {
                                let mut it = tail.iter();
                                while let Some(u) = it.next() {
                                    self.control.check()?;
                                    let ch = char_from_u16(u);
                                    let ccc = ccc_from_trie_value(self.trie.get(ch));
                                    if ccc != CanonicalCombiningClass::NotReordered {
                                        combining_characters
                                            .push(CharacterAndClass::new(ch, ccc))?;
                                        continue;
                                    }

                                    self.maybe_gather_combining()?;

                                    while let Some(u) = it.next_back() {
                                        self.control.check()?;
                                        let tail_char = char_from_u16(u);
                                        let trie_value = self.trie.get(tail_char);
                                        self.prepend_nonstarter_suffix(CharacterAndClassAndTrieValue::new_with_non_special_decomposition_trie_val(tail_char, trie_value))?;
                                    }
                                    self.prepend_nonstarter_suffix(CharacterAndClassAndTrieValue::new_with_non_decomposing_starter(ch))?;
                                    break;
                                }
                            }
                        } else {
                            let len = (len_bits + 1) as usize;
                            let offset32 = offset - self.scalars16.len();
                            let (starter, tail) = self
                                .scalars32
                                .get_subslice(offset32..offset32 + len)
                                .and_then(|slice| slice.split_first())
                                .unwrap_or_else(|| {
                                    debug_assert!(false);
                                    (REPLACEMENT_CHARACTER, EMPTY_CHAR)
                                });

                            c = starter;
                            if only_non_starters_in_trail {
                                for ch in tail.iter() {
                                    self.control.check()?;
                                    let trie_value = self.trie.get(ch);
                                    let ccc = ccc_from_trie_value(trie_value);
                                    combining_characters.push(CharacterAndClass::new(ch, ccc))?;
                                }
                            } else {
                                let mut it = tail.iter();
                                while let Some(ch) = it.next() {
                                    self.control.check()?;
                                    let ccc = ccc_from_trie_value(self.trie.get(ch));
                                    if ccc != CanonicalCombiningClass::NotReordered {
                                        combining_characters
                                            .push(CharacterAndClass::new(ch, ccc))?;
                                        continue;
                                    }
                                    self.maybe_gather_combining()?;

                                    while let Some(tail_char) = it.next_back() {
                                        self.control.check()?;
                                        let trie_value = self.trie.get(tail_char);
                                        self.prepend_nonstarter_suffix(CharacterAndClassAndTrieValue::new_with_non_special_decomposition_trie_val(tail_char, trie_value))?;
                                    }
                                    self.prepend_nonstarter_suffix(CharacterAndClassAndTrieValue::new_with_non_decomposing_starter(ch))?;
                                    break;
                                }
                            }
                        }
                        ce32 = data.ce32_for_char(c);
                        if ce32 == FALLBACK_CE32 {
                            data = self.root;
                            ce32 = data.ce32_for_char(c);
                        }
                    }
                }
            } else {
                let l = hangul_offset / HANGUL_N_COUNT;
                let v = (hangul_offset % HANGUL_N_COUNT) / HANGUL_T_COUNT;
                let t = hangul_offset % HANGUL_T_COUNT;

                self.mark_prefix_unmatchable();

                if self.is_next_decomposition_starts_with_starter() {
                    self.pending.push(
                        CollationElement32::new_from_ule(
                            self.jamo[(HANGUL_V_BASE - HANGUL_L_BASE + v) as usize],
                        )
                        .to_ce_self_contained_or_gigo(),
                    )?;
                    if t != 0 {
                        self.pending.push(
                            CollationElement32::new_from_ule(
                                self.jamo[(HANGUL_T_BASE - HANGUL_L_BASE + t) as usize],
                            )
                            .to_ce_self_contained_or_gigo(),
                        )?;
                    }
                    return Ok(CollationElement32::new_from_ule(self.jamo[l as usize])
                        .to_ce_self_contained_or_gigo());
                }

                if t != 0 {
                    self.pending.push(
                        CollationElement32::new_from_ule(
                            self.jamo[(HANGUL_V_BASE - HANGUL_L_BASE + v) as usize],
                        )
                        .to_ce_self_contained_or_gigo(),
                    )?;
                    self.upcoming.insert(
                        0,
                        CharacterAndClassAndTrieValue::new_with_non_decomposing_starter(
                            char_from_u32(HANGUL_T_BASE + t),
                        ),
                    )?;
                } else {
                    self.upcoming.insert(
                        0,
                        CharacterAndClassAndTrieValue::new_with_non_decomposing_starter(
                            char_from_u32(HANGUL_V_BASE + v),
                        ),
                    )?;
                }

                return Ok(CollationElement32::new_from_ule(self.jamo[l as usize])
                    .to_ce_self_contained_or_gigo());
            }
            let mut may_have_contracted_starter = false;
            self.collect_combining(&mut combining_characters)?;
            let mut looked_ahead = 0;
            let mut drain_from_upcoming = 0;
            'outer: loop {
                self.control.check()?;
                'ce32loop: loop {
                    self.control.check()?;
                    if let Some(ce) = ce32.to_ce_self_contained() {
                        self.pending.push(ce)?;
                        break 'ce32loop;
                    }
                    match ce32.tag() {
                        Tag::Expansion32 => {
                            let ce32s = data.get_ce32s(ce32.index(), ce32.len());
                            for u in ce32s.iter() {
                                self.control.check()?;
                                self.pending.push(
                                    CollationElement32::new(u).to_ce_self_contained_or_gigo(),
                                )?;
                            }
                            break 'ce32loop;
                        }
                        Tag::Expansion => {
                            let ces = data.get_ces(ce32.index(), ce32.len());
                            for u in ces.iter() {
                                self.control.check()?;
                                self.pending.push(CollationElement::new(u))?;
                            }
                            break 'ce32loop;
                        }
                        Tag::Prefix => {
                            let (default, mut trie) = data.get_default_and_trie(ce32.index());
                            ce32 = default;
                            for &ch in &self.prefix {
                                self.control.check()?;
                                match trie.next(ch) {
                                    TrieResult::NoValue => {}
                                    TrieResult::NoMatch => {
                                        continue 'ce32loop;
                                    }
                                    TrieResult::Intermediate(ce32_i) => {
                                        ce32 = CollationElement32::new(ce32_i as u32);
                                    }
                                    TrieResult::FinalValue(ce32_i) => {
                                        ce32 = CollationElement32::new(ce32_i as u32);
                                        continue 'ce32loop;
                                    }
                                }
                            }
                        }
                        Tag::Contraction => {
                            let every_suffix_starts_with_combining =
                                ce32.every_suffix_starts_with_combining();
                            let at_least_one_suffix_contains_starter =
                                ce32.at_least_one_suffix_contains_starter();
                            let at_least_one_suffix_ends_with_non_starter =
                                ce32.at_least_one_suffix_ends_with_non_starter();
                            let (default, mut trie) = data.get_default_and_trie(ce32.index());
                            ce32 = default;
                            if every_suffix_starts_with_combining && combining_characters.is_empty()
                            {
                                continue 'ce32loop;
                            }
                            let mut longest_matching_state = trie.clone();
                            let mut longest_matching_index = 0;
                            let mut attempt = 0;
                            let mut i = 0;
                            let mut most_recent_skipped_ccc = CanonicalCombiningClass::NotReordered;
                            let mut pending_removals: ProductionSmallVec<
                                'control,
                                [usize; PENDING_REMOVALS_SIZE],
                            > = ProductionSmallVec::new(self.control);
                            while let Some((character, ccc)) =
                                combining_characters.get(i).map(|c| c.character_and_ccc())
                            {
                                self.control.check()?;
                                match (most_recent_skipped_ccc < ccc, trie.next(character)) {
                                    (true, TrieResult::Intermediate(ce32_i)) => {
                                        let _ = combining_characters.remove(i)?;
                                        while let Some(idx) = pending_removals.pop() {
                                            self.control.check()?;
                                            combining_characters.remove(idx)?;
                                            i -= 1; // Adjust for the shortening
                                        }
                                        attempt = 0;
                                        longest_matching_index = i;
                                        longest_matching_state = trie.clone();
                                        ce32 = CollationElement32::new(ce32_i as u32);
                                    }
                                    (true, TrieResult::FinalValue(ce32_i)) => {
                                        let _ = combining_characters.remove(i)?;
                                        while let Some(idx) = pending_removals.pop() {
                                            self.control.check()?;
                                            combining_characters.remove(idx)?;
                                        }
                                        ce32 = CollationElement32::new(ce32_i as u32);
                                        continue 'ce32loop;
                                    }
                                    (_, TrieResult::NoValue) => {
                                        pending_removals.push(i)?;
                                        i += 1;
                                    }
                                    _ => {
                                        pending_removals.clear();
                                        most_recent_skipped_ccc = ccc;
                                        attempt += 1;
                                        i = longest_matching_index + attempt;
                                        trie = longest_matching_state.clone();
                                    }
                                }
                            }
                            if !(at_least_one_suffix_contains_starter
                                && combining_characters.is_empty())
                            {
                                continue 'ce32loop;
                            }
                            may_have_contracted_starter = true;
                            debug_assert!(pending_removals.is_empty());
                            self.ensure_upcoming_decomposed()?;
                            loop {
                                self.control.check()?;
                                let ahead = self.look_ahead(looked_ahead)?;
                                looked_ahead += 1;
                                if let Some(ch) = ahead {
                                    match trie.next(ch.character()) {
                                        TrieResult::NoValue => {}
                                        TrieResult::NoMatch => {
                                            if !at_least_one_suffix_ends_with_non_starter {
                                                continue 'ce32loop;
                                            }
                                            if !ch.decomposition_starts_with_non_starter() {
                                                continue 'ce32loop;
                                            }
                                            trie = longest_matching_state.clone();
                                            let mut longest_matching_index = 0;
                                            let mut attempt = 0;
                                            let mut i = 0;
                                            most_recent_skipped_ccc = ch.ccc();
                                            self.ensure_upcoming_decomposed()?;
                                            loop {
                                                self.control.check()?;
                                                let ahead = self.look_ahead(looked_ahead + i)?;
                                                if let Some(ch) = ahead {
                                                    let ccc = ch.ccc();
                                                    if ccc == CanonicalCombiningClass::NotReordered
                                                    {
                                                        continue 'ce32loop;
                                                    }
                                                    match (
                                                        most_recent_skipped_ccc < ccc,
                                                        trie.next(ch.character()),
                                                    ) {
                                                        (
                                                            true,
                                                            TrieResult::Intermediate(ce32_i),
                                                        ) => {
                                                            let _ = self
                                                                .upcoming
                                                                .remove(looked_ahead + i)?;
                                                            while let Some(idx) =
                                                                pending_removals.pop()
                                                            {
                                                                self.control.check()?;
                                                                self.upcoming
                                                                    .remove(looked_ahead + idx)?;
                                                                i -= 1; // Adjust for the shortening
                                                            }
                                                            attempt = 0;
                                                            longest_matching_index = i;
                                                            longest_matching_state = trie.clone();
                                                            ce32 = CollationElement32::new(
                                                                ce32_i as u32,
                                                            );
                                                        }
                                                        (true, TrieResult::FinalValue(ce32_i)) => {
                                                            let _ = self
                                                                .upcoming
                                                                .remove(looked_ahead + i)?;
                                                            while let Some(idx) =
                                                                pending_removals.pop()
                                                            {
                                                                self.control.check()?;
                                                                self.upcoming
                                                                    .remove(looked_ahead + idx)?;
                                                            }
                                                            ce32 = CollationElement32::new(
                                                                ce32_i as u32,
                                                            );
                                                            continue 'ce32loop;
                                                        }
                                                        (_, TrieResult::NoValue) => {
                                                            pending_removals.push(i)?;
                                                            i += 1;
                                                        }
                                                        _ => {
                                                            pending_removals.clear();
                                                            most_recent_skipped_ccc = ccc;
                                                            attempt += 1;
                                                            i = longest_matching_index + attempt;
                                                            trie = longest_matching_state.clone();
                                                        }
                                                    }
                                                } else {
                                                    continue 'ce32loop;
                                                }
                                            }
                                        }
                                        TrieResult::Intermediate(ce32_i) => {
                                            longest_matching_state = trie.clone();
                                            drain_from_upcoming = looked_ahead;
                                            ce32 = CollationElement32::new(ce32_i as u32);
                                        }
                                        TrieResult::FinalValue(ce32_i) => {
                                            drain_from_upcoming = looked_ahead;
                                            ce32 = CollationElement32::new(ce32_i as u32);
                                            continue 'ce32loop;
                                        }
                                    }
                                } else {
                                    continue 'ce32loop;
                                }
                            }
                        }
                        Tag::Digit => {
                            ce32 = data.get_ce32(ce32.index());
                        }
                        Tag::Offset => {
                            self.pending.push(data.ce_from_offset_ce32(c, ce32))?;
                            break 'ce32loop;
                        }
                        Tag::Implicit => {
                            self.pending
                                .push(CollationElement::new_implicit_from_char(c))?;
                            break 'ce32loop;
                        }
                        Tag::Fallback
                        | Tag::Reserved3
                        | Tag::LongPrimary
                        | Tag::LongSecondary
                        | Tag::BuilderData
                        | Tag::LeadSurrogate
                        | Tag::LatinExpansion
                        | Tag::U0000
                        | Tag::Hangul => {
                            debug_assert!(false);
                            self.pending.push(FFFD_CE)?;
                            break 'ce32loop;
                        }
                    }
                }
                self.prefix_push(c);
                'combining_outer: loop {
                    self.control.check()?;
                    debug_assert!(drain_from_upcoming == 0 || combining_characters.is_empty());
                    let mut i = 0;
                    'combining: while let Some(ch) =
                        combining_characters.get(i).map(|c| c.character())
                    {
                        self.control.check()?;
                        c = ch;
                        let diacritic_index = (c as usize).wrapping_sub(COMBINING_DIACRITICS_BASE);
                        if let Some(secondary) = self.diacritics.get(diacritic_index) {
                            self.pending
                                .push(CollationElement::new_from_secondary(secondary))?;
                            self.mark_prefix_unmatchable();
                            i += 1;
                            continue 'combining;
                        }
                        combining_characters.remove_prefix(i + 1)?;
                        data = self.root;
                        ce32 = data.ce32_for_char(c);
                        if ce32 == FALLBACK_CE32 {
                            data = self.root;
                            ce32 = data.ce32_for_char(c);
                        }
                        continue 'outer;
                    }
                    i = 0;
                    while i < drain_from_upcoming {
                        self.control.check()?;

                        let ch = self.upcoming[i].character();
                        self.prefix_push(ch);
                        i += 1;
                    }
                    self.upcoming.remove_prefix(drain_from_upcoming)?;
                    if self.upcoming.is_empty() {
                        #[cfg(debug_assertions)]
                        debug_assert!(self.iter_exhausted || may_have_contracted_starter);
                        if let Some(c_c_tv) = self.iter_next()? {
                            self.upcoming.push(c_c_tv)?;
                        } else {
                            #[cfg(debug_assertions)]
                            {
                                self.iter_exhausted = true;
                            }
                        }
                    }
                    if may_have_contracted_starter {
                        may_have_contracted_starter = false;
                        if !self.is_next_decomposition_starts_with_starter() {
                            drain_from_upcoming = 0;
                            self.collect_combining(&mut combining_characters)?;
                            continue 'combining_outer;
                        }
                    }

                    let ret = self.pending[0];
                    debug_assert_eq!(self.pending_pos, 0);
                    if self.pending.len() == 1 {
                        self.pending.clear();
                    } else {
                        self.pending_pos = 1;
                    }
                    return Ok(ret);
                }
            }
        } else {
            NO_CE
        })
    }
}
