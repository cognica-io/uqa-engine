//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

// Adapted from ICU4X 2.0.0 (Unicode-3.0); see LICENSE-ICU and PROVENANCE.md.

use core::char::REPLACEMENT_CHARACTER;
use icu_collections::codepointtrie::CodePointTrie;
use icu_properties::props::CanonicalCombiningClass;
use zerovec::{
    ule::{AsULE, RawBytesULE},
    zeroslice, ZeroSlice,
};

pub(crate) const CE_BUFFER_SIZE: usize = 8;

pub(super) const PENDING_CE_BUFFER_SIZE: usize = 6;

pub(super) const UPCOMING_CHARACTER_BUFFER_SIZE: usize = 10;

pub(super) const COMBINING_CHARACTER_BUFFER_SIZE: usize = 7;

pub(super) const PENDING_REMOVALS_SIZE: usize = 1;

pub(crate) const NON_ROUND_TRIP_MARKER: u32 = 1 << 30;

pub(crate) const BACKWARD_COMBINING_MARKER: u32 = 1 << 31;

pub(crate) const HIGH_ZEROS_MASK: u32 = 0x3FFF_0000;

pub(crate) const LOW_ZEROS_MASK: u32 = 0xFFE0;

pub(super) const FDFA_MARKER: u16 = 1;

pub(crate) const HANGUL_SYLLABLE_MARKER: u32 = 1;

pub(super) fn trie_value_has_ccc(trie_value: u32) -> bool {
    (trie_value & 0x3FFF_FE00) == 0xD800
}

pub(super) fn trie_value_indicates_special_non_starter_decomposition(trie_value: u32) -> bool {
    (trie_value & 0x3FFF_FF00) == 0xD900
}

pub(super) fn decomposition_starts_with_non_starter(trie_value: u32) -> bool {
    trie_value_has_ccc(trie_value)
}

pub(super) fn ccc_from_trie_value(trie_value: u32) -> CanonicalCombiningClass {
    if trie_value_has_ccc(trie_value) {
        CanonicalCombiningClass::from_icu4c_value(trie_value as u8)
    } else {
        CanonicalCombiningClass::NotReordered
    }
}

pub(super) const HANGUL_S_BASE: u32 = 0xAC00;
pub(super) const HANGUL_L_BASE: u32 = 0x1100;
pub(super) const HANGUL_V_BASE: u32 = 0x1161;
pub(super) const HANGUL_T_BASE: u32 = 0x11A7;
pub(super) const HANGUL_T_COUNT: u32 = 28;
pub(super) const HANGUL_N_COUNT: u32 = 588;
pub(super) const HANGUL_S_COUNT: u32 = 11172;

pub(crate) const JAMO_COUNT: usize = 256; // 0x1200 - 0x1100

pub(super) const COMBINING_DIACRITICS_BASE: usize = 0x0300;

pub(crate) const CASE_MASK: u16 = 0xC000;
pub(crate) const TERTIARY_MASK: u16 = 0x3F3F; // ONLY_TERTIARY_MASK in ICU4C
pub(crate) const QUATERNARY_MASK: u16 = 0xC0;

pub(super) const SPECIAL_CE32_LOW_BYTE: u8 = 0xC0;
pub(crate) const FALLBACK_CE32: CollationElement32 =
    CollationElement32(SPECIAL_CE32_LOW_BYTE as u32);
pub(super) const LONG_PRIMARY_CE32_LOW_BYTE: u8 = 0xC1; // SPECIAL_CE32_LOW_BYTE | LONG_PRIMARY_TAG
pub(super) const COMMON_SECONDARY_CE: u64 = 0x0500_0000;
pub(super) const COMMON_TERTIARY_CE: u64 = 0x0500;
pub(super) const COMMON_SEC_AND_TER_CE: u64 = COMMON_SECONDARY_CE | COMMON_TERTIARY_CE;

pub(super) const UNASSIGNED_IMPLICIT_BYTE: u8 = 0xFE;

pub(super) const CONTRACT_NEXT_CCC: u32 = 0x200;
pub(super) const CONTRACT_TRAILING_CCC: u32 = 0x400;
pub(super) const CONTRACT_HAS_STARTER: u32 = 0x800;

pub(crate) const NO_CE: CollationElement = CollationElement::default();
pub(crate) const NO_CE_PRIMARY: u32 = 1; // not a left-adjusted weight
pub(crate) const NO_CE_SECONDARY: u16 = 0x0100;
pub(crate) const NO_CE_TERTIARY: u16 = 0x0100;
pub(super) const NO_CE_VALUE: u64 =
    ((NO_CE_PRIMARY as u64) << 32) | ((NO_CE_SECONDARY as u64) << 16) | (NO_CE_TERTIARY as u64); // 0x1_0100_0100

pub(super) const FFFD_PRIMARY: u32 = 0xFFFD_0000; // U+FFFD
pub(crate) const FFFD_CE_VALUE: u64 = ((FFFD_PRIMARY as u64) << 32) | COMMON_SEC_AND_TER_CE;
pub(crate) const FFFD_CE: CollationElement = CollationElement(FFFD_CE_VALUE);
pub(crate) const FFFD_CE32_VALUE: u32 = 0xFFFD_0505;
pub(crate) const FFFD_CE32: CollationElement32 = CollationElement32(FFFD_CE32_VALUE);

pub(crate) const EMPTY_U16: &ZeroSlice<u16> = zeroslice![];
pub(super) const SINGLE_REPLACEMENT_CHARACTER_U16: &ZeroSlice<u16> =
    zeroslice!(u16; <u16 as AsULE>::ULE::from_unsigned; [REPLACEMENT_CHARACTER as u16]);

pub(crate) const EMPTY_CHAR: &ZeroSlice<char> = zeroslice![];
pub(super) const SINGLE_REPLACEMENT_CHARACTER_CHAR: &ZeroSlice<char> =
    zeroslice!(char; <char as AsULE>::ULE::from_aligned; [REPLACEMENT_CHARACTER]);

pub(crate) fn unwrap_or_gigo<T>(opt: Option<T>, default: T) -> T {
    if let Some(val) = opt {
        val
    } else {
        debug_assert!(false);
        default
    }
}

pub(crate) fn char_from_u32(u: u32) -> char {
    unwrap_or_gigo(core::char::from_u32(u), REPLACEMENT_CHARACTER)
}

pub(super) fn char_from_u16(u: u16) -> char {
    char_from_u32(u32::from(u))
}

pub(super) fn in_inclusive_range(c: char, start: char, end: char) -> bool {
    u32::from(c).wrapping_sub(u32::from(start)) <= (u32::from(end) - u32::from(start))
}

#[derive(Eq, PartialEq, Debug)]
#[repr(u8)]
pub(crate) enum Tag {
    Fallback = 0,
    LongPrimary = 1,
    LongSecondary = 2,
    Reserved3 = 3,
    LatinExpansion = 4,
    Expansion32 = 5,
    Expansion = 6,
    BuilderData = 7,
    Prefix = 8,
    Contraction = 9,
    Digit = 10,
    U0000 = 11,
    Hangul = 12,
    LeadSurrogate = 13,
    Offset = 14,
    Implicit = 15,
}

#[derive(Copy, Clone, PartialEq, Debug)]
pub(crate) struct CollationElement32(u32);

impl CollationElement32 {
    pub fn new(bits: u32) -> Self {
        CollationElement32(bits)
    }

    pub fn new_from_ule(ule: RawBytesULE<4>) -> Self {
        CollationElement32(u32::from_unaligned(ule))
    }

    fn low_byte(self) -> u8 {
        self.0 as u8
    }

    pub(crate) fn tag(self) -> Tag {
        debug_assert!(self.low_byte() >= SPECIAL_CE32_LOW_BYTE);
        match self.low_byte() & 0xF {
            0 => Tag::Fallback,
            1 => Tag::LongPrimary,
            2 => Tag::LongSecondary,
            3 => Tag::Reserved3,
            4 => Tag::LatinExpansion,
            5 => Tag::Expansion32,
            6 => Tag::Expansion,
            7 => Tag::BuilderData,
            8 => Tag::Prefix,
            9 => Tag::Contraction,
            10 => Tag::Digit,
            11 => Tag::U0000,
            12 => Tag::Hangul,
            13 => Tag::LeadSurrogate,
            14 => Tag::Offset,
            _ => Tag::Implicit,
        }
    }

    pub fn to_ce_simple_or_long_primary(self) -> Option<CollationElement> {
        let t = self.low_byte();
        if t < SPECIAL_CE32_LOW_BYTE {
            let as64 = u64::from(self.0);
            Some(CollationElement::new(
                ((as64 & 0xFFFF_0000) << 32) | ((as64 & 0xFF00) << 16) | (u64::from(t) << 8),
            ))
        } else if t == LONG_PRIMARY_CE32_LOW_BYTE {
            let as64 = u64::from(self.0);
            Some(CollationElement::new(
                ((as64 - u64::from(t)) << 32) | COMMON_SEC_AND_TER_CE,
            ))
        } else {
            None
        }
    }

    pub fn to_ce_self_contained(self) -> Option<CollationElement> {
        if let Some(ce) = self.to_ce_simple_or_long_primary() {
            return Some(ce);
        }
        if self.tag() == Tag::LongSecondary {
            Some(CollationElement::new(u64::from(self.0 & 0xffff_ff00)))
        } else {
            None
        }
    }

    pub fn to_ce_self_contained_or_gigo(self) -> CollationElement {
        unwrap_or_gigo(self.to_ce_self_contained(), FFFD_CE)
    }

    pub fn len(self) -> usize {
        debug_assert!(self.tag() == Tag::Expansion32 || self.tag() == Tag::Expansion);
        ((self.0 >> 8) & 31) as usize
    }

    pub fn index(self) -> usize {
        debug_assert!(
            self.tag() == Tag::Expansion32
                || self.tag() == Tag::Expansion
                || self.tag() == Tag::Contraction
                || self.tag() == Tag::Digit
                || self.tag() == Tag::Prefix
                || self.tag() == Tag::Offset
        );
        (self.0 >> 13) as usize
    }

    pub fn every_suffix_starts_with_combining(self) -> bool {
        debug_assert!(self.tag() == Tag::Contraction);
        (self.0 & CONTRACT_NEXT_CCC) != 0
    }
    pub fn at_least_one_suffix_contains_starter(self) -> bool {
        debug_assert!(self.tag() == Tag::Contraction);
        (self.0 & CONTRACT_HAS_STARTER) != 0
    }
    pub fn at_least_one_suffix_ends_with_non_starter(self) -> bool {
        debug_assert!(self.tag() == Tag::Contraction);
        (self.0 & CONTRACT_TRAILING_CCC) != 0
    }
}

impl Default for CollationElement32 {
    fn default() -> Self {
        CollationElement32(1) // NO_CE32
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct CollationElement(u64);

impl CollationElement {
    pub fn new(bits: u64) -> Self {
        CollationElement(bits)
    }

    pub fn new_from_primary(primary: u32) -> Self {
        CollationElement((u64::from(primary) << 32) | COMMON_SEC_AND_TER_CE)
    }

    pub fn new_from_secondary(secondary: u16) -> Self {
        CollationElement((u64::from(secondary) << 16) | COMMON_TERTIARY_CE)
    }

    pub fn new_implicit_from_char(c: char) -> Self {
        let mut c_with_offset = u32::from(c) + 1;
        let mut primary: u32 = 2 + (c_with_offset % 18) * 14;
        c_with_offset /= 18;
        primary |= (2 + (c_with_offset % 254)) << 8;
        c_with_offset /= 254;
        primary |= (4 + (c_with_offset % 251)) << 16;
        primary |= u32::from(UNASSIGNED_IMPLICIT_BYTE) << 24;
        CollationElement::new_from_primary(primary)
    }

    pub fn primary(self) -> u32 {
        (self.0 >> 32) as u32
    }

    pub fn non_primary(self) -> NonPrimary {
        NonPrimary::new(self.0 as u32)
    }

    pub fn secondary(self) -> u16 {
        self.non_primary().secondary()
    }

    pub const fn default() -> CollationElement {
        CollationElement(NO_CE_VALUE) // NO_CE
    }
}

impl Default for CollationElement {
    fn default() -> Self {
        CollationElement(NO_CE_VALUE) // NO_CE
    }
}

impl Default for &CollationElement {
    fn default() -> Self {
        &CollationElement(NO_CE_VALUE) // NO_CE
    }
}

#[derive(Copy, Clone, PartialEq, Debug)]
pub(crate) struct NonPrimary(u32);

impl NonPrimary {
    pub fn new(bits: u32) -> Self {
        NonPrimary(bits)
    }

    pub fn secondary(self) -> u16 {
        (self.0 >> 16) as u16
    }

    pub fn tertiary_case_quarternary(self, mask: u16) -> u16 {
        debug_assert!((mask & CASE_MASK) == CASE_MASK || (mask & CASE_MASK) == 0);
        debug_assert!((mask & TERTIARY_MASK) == TERTIARY_MASK || (mask & TERTIARY_MASK) == 0);
        debug_assert!((mask & QUATERNARY_MASK) == QUATERNARY_MASK || (mask & QUATERNARY_MASK) == 0);
        (self.0 as u16) & mask
    }
}

impl Default for NonPrimary {
    fn default() -> Self {
        NonPrimary(0x0100_0100) // Low 32 bits of NO_CE
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CharacterAndClassAndTrieValue {
    pub(super) c_and_c: CharacterAndClass,
    pub trie_val: u32,
}

impl CharacterAndClassAndTrieValue {
    pub fn new_with_non_decomposing_starter(c: char) -> Self {
        CharacterAndClassAndTrieValue {
            c_and_c: CharacterAndClass::new(c, CanonicalCombiningClass::NotReordered),
            trie_val: 0,
        }
    }
    pub fn new_with_non_zero_ccc(c: char, ccc: CanonicalCombiningClass) -> Self {
        CharacterAndClassAndTrieValue {
            c_and_c: CharacterAndClass::new(c, ccc),
            trie_val: 0xD800 | u32::from(ccc.to_icu4c_value()),
        }
    }
    pub fn new_with_non_special_decomposition_trie_val(c: char, trie_val: u32) -> Self {
        debug_assert!(!trie_value_indicates_special_non_starter_decomposition(
            trie_val
        ));
        CharacterAndClassAndTrieValue {
            c_and_c: CharacterAndClass::new_with_trie_value(c, trie_val),
            trie_val,
        }
    }
    pub fn new_with_trie_val(c: char, trie_val: u32) -> Self {
        if trie_value_indicates_special_non_starter_decomposition(trie_val) {
            CharacterAndClassAndTrieValue {
                c_and_c: CharacterAndClass::new(c, CanonicalCombiningClass::from_icu4c_value(0xFF)),
                trie_val,
            }
        } else {
            CharacterAndClassAndTrieValue {
                c_and_c: CharacterAndClass::new_with_trie_value(c, trie_val),
                trie_val,
            }
        }
    }

    pub fn decomposition_starts_with_non_starter(self) -> bool {
        decomposition_starts_with_non_starter(self.trie_val)
    }

    pub fn character(self) -> char {
        self.c_and_c.character()
    }

    pub(super) fn ccc(self) -> CanonicalCombiningClass {
        let ret = self.c_and_c.ccc();
        debug_assert_ne!(ret, CanonicalCombiningClass::from_icu4c_value(0xFF));
        ret
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct CharacterAndClass(u32);

impl CharacterAndClass {
    pub fn new(c: char, ccc: CanonicalCombiningClass) -> Self {
        CharacterAndClass(u32::from(c) | (u32::from(ccc.to_icu4c_value()) << 24))
    }
    pub fn new_with_placeholder(c: char) -> Self {
        CharacterAndClass(u32::from(c) | ((0xFF) << 24))
    }
    pub fn new_with_trie_value(c: char, trie_value: u32) -> Self {
        Self::new(c, ccc_from_trie_value(trie_value))
    }
    pub fn character(self) -> char {
        char_from_u32(self.0 & 0xFF_FFFF)
    }
    pub fn ccc(self) -> CanonicalCombiningClass {
        CanonicalCombiningClass::from_icu4c_value((self.0 >> 24) as u8)
    }
    pub fn character_and_ccc(self) -> (char, CanonicalCombiningClass) {
        (self.character(), self.ccc())
    }
    pub fn set_ccc_from_trie_if_not_already_set(&mut self, trie: &CodePointTrie<u32>) {
        if self.0 >> 24 != 0xFF {
            return;
        }
        let scalar = self.0 & 0xFF_FFFF;
        self.0 =
            ((ccc_from_trie_value(trie.get32_u32(scalar)).to_icu4c_value() as u32) << 24) | scalar;
    }
}
