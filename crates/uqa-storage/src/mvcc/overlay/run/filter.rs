//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Charged Bloom filters reject absent keys and literal prefixes without reading a run.

use uqa_core::memory::{BudgetedVec, MemoryBudget};

const BITS_PER_KEY: u64 = 10;
const HASHES: u64 = 7;
const FIRST_SEED: u64 = 0xcbf2_9ce4_8422_2325;
const SECOND_SEED: u64 = 0x8422_2325_cbf2_9ce4;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// A Bloom filter of `HASHES` probes, which errs only by reporting an absent inserted identity as possibly present.
pub(super) struct KeyFilter {
    words: BudgetedVec<u64>,
    bits: u64,
}

impl KeyFilter {
    /// A filter for `keys` keys, or `None` when it would occupy more than a quarter of the available workspace needed by the run index and readers. Omitting it makes lookups slower but exact.
    pub(super) fn with_capacity(keys: u64, memory: &MemoryBudget) -> Option<Self> {
        let bits = keys
            .checked_mul(BITS_PER_KEY)?
            .max(64)
            .checked_next_multiple_of(64)?;
        let words = usize::try_from(bits / 64).ok()?;
        let maximum = memory.available() / 4;
        Self::allocate(words, maximum, memory)
    }

    /// Index all literal prefixes when their estimated filter fits the same optional workspace policy as point keys. The key-byte estimate bounds the possible prefix count; pressure omits the filter instead of retaining a saturated allocation.
    pub(super) fn for_prefixes(key_bytes: u64, memory: &MemoryBudget) -> Option<Self> {
        Self::with_capacity(key_bytes, memory)
    }

    fn allocate(words: usize, maximum: usize, memory: &MemoryBudget) -> Option<Self> {
        if words == 0 || words.checked_mul(size_of::<u64>())? > maximum {
            return None;
        }
        let mut storage = BudgetedVec::new(memory);
        storage.reserve(words).ok()?;
        if storage.capacity().checked_mul(size_of::<u64>())? > maximum {
            return None;
        }
        for _ in 0..words {
            storage.push(0).ok()?;
        }
        Some(Self {
            words: storage,
            bits: (words as u64).checked_mul(64)?,
        })
    }

    pub(super) fn insert(&mut self, key: &[u8]) {
        self.insert_probes(probes(key));
    }

    /// Extend rolling hashes once per byte and insert only prefixes not shared with the previous sorted key. This remains linear in key bytes even for long identities.
    pub(super) fn insert_prefixes(&mut self, key: &[u8], previous: Option<&[u8]>) {
        let shared = previous.map_or(0, |previous| {
            key.iter().zip(previous).take_while(|(a, b)| a == b).count()
        });
        let (mut first, mut second) = (FIRST_SEED, SECOND_SEED);
        for (index, byte) in key.iter().enumerate() {
            first = (first ^ u64::from(*byte)).wrapping_mul(FNV_PRIME);
            second = (second ^ u64::from(*byte)).wrapping_mul(FNV_PRIME);
            if index >= shared {
                self.insert_probes((finish_hash(first), finish_hash(second) | 1));
            }
        }
    }

    fn insert_probes(&mut self, (first, second): (u64, u64)) {
        for probe in 0..HASHES {
            let bit = first.wrapping_add(probe.wrapping_mul(second)) % self.bits;
            self.words[(bit / 64) as usize] |= 1 << (bit % 64);
        }
    }

    pub(super) fn may_contain(&self, key: &[u8]) -> bool {
        let (first, second) = probes(key);
        (0..HASHES).all(|probe| {
            let bit = first.wrapping_add(probe.wrapping_mul(second)) % self.bits;
            self.words[(bit / 64) as usize] & (1 << (bit % 64)) != 0
        })
    }
}

/// Two independent hashes of `key` for double hashing; the second is odd, so the probes cover the filter.
fn probes(key: &[u8]) -> (u64, u64) {
    (hash(key, FIRST_SEED), hash(key, SECOND_SEED) | 1)
}

/// FNV-1a from `seed`, finished with the `MurmurHash3` 64-bit mixer so that every input bit affects every output bit.
fn hash(key: &[u8], seed: u64) -> u64 {
    let mut state = seed;
    for byte in key {
        state ^= u64::from(*byte);
        state = state.wrapping_mul(FNV_PRIME);
    }
    finish_hash(state)
}

fn finish_hash(mut state: u64) -> u64 {
    state ^= state >> 33;
    state = state.wrapping_mul(0xff51_afd7_ed55_8ccd);
    state ^= state >> 33;
    state = state.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    state ^ (state >> 33)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_filters_preserve_literal_prefixes_with_bounded_shared_workspace() {
        let memory = MemoryBudget::new(64 << 10);
        let retained = memory.reserve(48 << 10).unwrap();
        let reader = memory.child(256 << 10);
        let mut keys = vec![
            Vec::new(),
            b"alpha".to_vec(),
            b"alphabet".to_vec(),
            vec![0, 255],
        ];
        keys.extend((0_u64..128).map(|id| {
            let mut key = b"vectors/".to_vec();
            key.extend_from_slice(&id.to_be_bytes());
            key.extend_from_slice(&[0, 255]);
            key
        }));
        keys.push(vec![255; 4096]);
        keys.sort();
        assert!(KeyFilter::for_prefixes(1 << 20, &reader).is_none());
        assert_eq!(memory.used(), retained.bytes());
        let admitted = KeyFilter::for_prefixes(128, &reader).unwrap();
        assert!(reader.used() <= (16 << 10) / 4);
        drop((admitted, retained));
        let available = reader.available();
        let mut filter =
            KeyFilter::for_prefixes(keys.iter().map(|key| key.len() as u64).sum(), &reader)
                .unwrap();
        for (index, key) in keys.iter().enumerate() {
            filter.insert_prefixes(
                key,
                index
                    .checked_sub(1)
                    .map(|previous| keys[previous].as_slice()),
            );
        }
        for key in &keys {
            for length in 1..=key.len() {
                assert!(
                    filter.may_contain(&key[..length]),
                    "missing prefix length {length}"
                );
            }
        }
        assert!(!filter.may_contain(b"not an inserted prefix"));
        assert!(reader.used() <= available / 4);
        drop(filter);
        assert_eq!(memory.used(), 0);
        assert!(KeyFilter::for_prefixes(128, &MemoryBudget::new(7)).is_none());
    }

    #[test]
    fn optional_filter_leaves_shared_ancestor_workspace_for_the_run_consumer() {
        let memory = MemoryBudget::new(64 << 10);
        let retained = memory.reserve(48 << 10).unwrap();
        let reader = memory.child(256 << 10);
        assert!(KeyFilter::with_capacity(8_192, &reader).is_none());
        assert_eq!(memory.used(), retained.bytes());

        let mut filter = KeyFilter::with_capacity(128, &reader).unwrap();
        for key in 0_u64..128 {
            filter.insert(&key.to_be_bytes());
        }
        for key in 0_u64..128 {
            assert!(filter.may_contain(&key.to_be_bytes()));
        }
        assert!(reader.used() <= (16 << 10) / 4);
        drop((filter, retained));
        assert_eq!(memory.used(), 0);
    }
}
