//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

// Adapted from ICU4X 2.0.0 (Unicode-3.0); see LICENSE-ICU and PROVENANCE.md.

use super::elements::{
    CollationElement, CollationElement32, Tag, EMPTY_U16, FFFD_CE, FFFD_CE32, FFFD_CE32_VALUE,
    FFFD_CE_VALUE,
};
use icu_collator::provider::CollationData;
use icu_collections::char16trie::Char16TrieIterator;
use zerovec::ZeroSlice;
const SINGLE_U32: &ZeroSlice<u32> =
    zerovec::zeroslice!(u32; <u32 as zerovec::ule::AsULE>::ULE::from_unsigned; [FFFD_CE32_VALUE]);
const SINGLE_U64: &ZeroSlice<u64> =
    zerovec::zeroslice!(u64; <u64 as zerovec::ule::AsULE>::ULE::from_unsigned; [FFFD_CE_VALUE]);
pub(super) trait CollationDataExt<'data> {
    fn ce32_for_char(&self, c: char) -> CollationElement32;
    fn get_ce32(&'data self, index: usize) -> CollationElement32;
    fn get_ce32s(&'data self, index: usize, len: usize) -> &'data ZeroSlice<u32>;
    fn get_ces(&'data self, index: usize, len: usize) -> &'data ZeroSlice<u64>;
    fn get_default_and_trie_impl(
        &'data self,
        index: usize,
    ) -> (CollationElement32, &'data ZeroSlice<u16>);
    fn get_default_and_trie(
        &'data self,
        index: usize,
    ) -> (CollationElement32, Char16TrieIterator<'data>);
    fn get_default(&'data self, index: usize) -> CollationElement32;
    fn ce_from_offset_ce32(&self, c: char, ce32: CollationElement32) -> CollationElement;
}
impl<'data> CollationDataExt<'data> for CollationData<'data> {
    fn ce32_for_char(&self, c: char) -> CollationElement32 {
        CollationElement32::new(self.trie.get32(c as u32))
    }
    fn get_ce32(&'data self, index: usize) -> CollationElement32 {
        CollationElement32::new(if let Some(u) = self.ce32s.get(index) {
            u
        } else {
            debug_assert!(false);
            FFFD_CE32_VALUE
        })
    }
    fn get_ce32s(&'data self, index: usize, len: usize) -> &'data ZeroSlice<u32> {
        if len > 0 {
            if let Some(slice) = self.ce32s.get_subslice(index..index + len) {
                return slice;
            }
        }
        debug_assert!(false);
        SINGLE_U32
    }
    fn get_ces(&'data self, index: usize, len: usize) -> &'data ZeroSlice<u64> {
        if len > 0 {
            if let Some(slice) = self.ces.get_subslice(index..index + len) {
                return slice;
            }
        }
        debug_assert!(false);
        SINGLE_U64
    }
    fn get_default_and_trie_impl(
        &'data self,
        index: usize,
    ) -> (CollationElement32, &'data ZeroSlice<u16>) {
        if let Some(slice) = self.contexts.get_subslice(index..self.contexts.len()) {
            #[allow(clippy::unwrap_used)]
            if slice.len() >= 2 {
                let first = slice.get(0).unwrap();
                let second = slice.get(1).unwrap();
                let trie = slice.get_subslice(2..slice.len()).unwrap();
                return (
                    CollationElement32::new((u32::from(first) << 16) | u32::from(second)),
                    trie,
                );
            }
        }
        debug_assert!(false);
        (FFFD_CE32, EMPTY_U16)
    }
    fn get_default_and_trie(
        &'data self,
        index: usize,
    ) -> (CollationElement32, Char16TrieIterator<'data>) {
        let (ce32, trie) = self.get_default_and_trie_impl(index);
        (ce32, Char16TrieIterator::new(trie))
    }
    fn get_default(&'data self, index: usize) -> CollationElement32 {
        let (ce32, _) = self.get_default_and_trie_impl(index);
        ce32
    }
    fn ce_from_offset_ce32(&self, c: char, ce32: CollationElement32) -> CollationElement {
        debug_assert!(ce32.tag() == Tag::Offset);
        if let Some(data_ce) = self.ces.get(ce32.index()) {
            CollationElement::new_from_primary(data_ce_to_primary(data_ce, c))
        } else {
            debug_assert!(false);
            FFFD_CE
        }
    }
}
fn data_ce_to_primary(data_ce: u64, c: char) -> u32 {
    let p = (data_ce >> 32) as u32; // three-byte primary pppppp00
    let lower32 = data_ce as u32 as i32; // base code point b & step s: bbbbbbss (bit 7: isCompressible)
    let mut offset = ((u32::from(c) as i32) - (lower32 >> 8)) * (lower32 & 0x7F); // delta * increment
    let is_compressible = (lower32 & 0x80) != 0;
    offset += (((p >> 8) & 0xFF) as i32) - 2;
    let mut primary = (((offset % 254) + 2) as u32) << 8;
    offset /= 254;
    if is_compressible {
        offset += (((p >> 16) & 0xFF) as i32) - 4;
        primary |= (((offset % 251) + 4) as u32) << 16;
        offset /= 251;
    } else {
        offset += (((p >> 16) & 0xFF) as i32) - 2;
        primary |= (((offset % 254) + 2) as u32) << 16;
        offset /= 254;
    }
    primary | ((p & 0xFF00_0000) + ((offset as u32) << 24))
}
