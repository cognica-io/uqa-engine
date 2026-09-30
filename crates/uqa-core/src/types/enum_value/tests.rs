//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn assert_strictly_increasing(keys: &[EnumLabelKey]) {
    for pair in keys.windows(2) {
        assert!(
            pair[0] < pair[1],
            "{:?} must precede {:?}",
            pair[0],
            pair[1]
        );
    }
    for key in keys {
        assert!(EnumLabelKey::from_bytes(key.as_bytes().to_vec()).is_ok());
    }
}

/// Deterministic generator so failures reproduce without an external crate.
struct Sequence(u64);

impl Sequence {
    fn next(&mut self, bound: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        usize::try_from(self.0 >> 33).unwrap() % bound
    }
}

#[test]
fn initial_keys_are_valid_increasing_and_short() {
    for count in [0, 1, 2, 3, 127, 128, 254, 255, 256, 1_000, 40_000] {
        let keys = EnumLabelKey::initial(count).unwrap();
        assert_eq!(keys.len(), count);
        assert_strictly_increasing(&keys);
        let width = keys
            .iter()
            .map(|key| key.as_bytes().len())
            .max()
            .unwrap_or(0);
        let expected = if count < 128 {
            1
        } else if count < 32_768 {
            2
        } else {
            3
        };
        assert!(width <= expected, "{count} labels used {width}-byte keys");
    }
    let keys = EnumLabelKey::initial(3).unwrap();
    let bytes: Vec<&[u8]> = keys.iter().map(EnumLabelKey::as_bytes).collect();
    assert_eq!(bytes, [&[64][..], &[128][..], &[192][..]]);
}

#[test]
fn insertions_stay_strictly_between_neighbors_without_changing_existing_keys() {
    for seed in 0..32 {
        let mut sequence = Sequence(seed);
        let mut keys = EnumLabelKey::initial(sequence.next(5)).unwrap();
        for _ in 0..400 {
            let position = sequence.next(keys.len() + 1);
            let lower = position.checked_sub(1).map(|index| &keys[index]);
            let upper = keys.get(position);
            let key = EnumLabelKey::between(lower, upper).unwrap();
            if let Some(lower) = lower {
                assert!(lower < &key);
            }
            if let Some(upper) = upper {
                assert!(&key < upper);
            }
            let before = keys.clone();
            keys.insert(position, key);
            assert_eq!(&keys[..position], &before[..position]);
            assert_eq!(&keys[position + 1..], &before[position..]);
        }
        assert_strictly_increasing(&keys);
    }
}

#[test]
fn appends_increment_the_shortest_prefix() {
    let mut keys = EnumLabelKey::initial(1).unwrap();
    for _ in 0..1_000 {
        let key = EnumLabelKey::between(keys.last(), None).unwrap();
        keys.push(key);
    }
    assert_strictly_increasing(&keys);
    assert!(keys.iter().all(|key| key.as_bytes().len() <= 5));
    let all_max = EnumLabelKey::from_bytes(vec![255, 255]).unwrap();
    assert_eq!(
        EnumLabelKey::between(Some(&all_max), None)
            .unwrap()
            .as_bytes(),
        [255, 255, 1]
    );
}

#[test]
fn repeated_insertion_at_one_position_grows_one_byte_per_eight_labels() {
    let first = EnumLabelKey::initial(2).unwrap();
    let mut upper = first[1].clone();
    let lower = first[0].clone();
    for _ in 0..800 {
        let key = EnumLabelKey::between(Some(&lower), Some(&upper)).unwrap();
        assert!(lower < key && key < upper);
        upper = key;
    }
    assert!(upper.as_bytes().len() <= 1 + 800 / 8 + 1);
    let mut lower = first[0].clone();
    let upper = first[1].clone();
    for _ in 0..800 {
        let key = EnumLabelKey::between(Some(&lower), Some(&upper)).unwrap();
        assert!(lower < key && key < upper);
        lower = key;
    }
    assert!(lower.as_bytes().len() <= 1 + 800 / 8 + 1);
}

#[test]
fn prepending_and_adjacent_insertion_use_midpoints() {
    let one = EnumLabelKey::from_bytes(vec![1]).unwrap();
    assert_eq!(
        EnumLabelKey::between(None, Some(&one)).unwrap().as_bytes(),
        [0, 128]
    );
    let left = EnumLabelKey::from_bytes(vec![7]).unwrap();
    let right = EnumLabelKey::from_bytes(vec![8]).unwrap();
    assert_eq!(
        EnumLabelKey::between(Some(&left), Some(&right))
            .unwrap()
            .as_bytes(),
        [7, 128]
    );
    let long = EnumLabelKey::from_bytes(vec![7, 255, 255]).unwrap();
    assert_eq!(
        EnumLabelKey::between(Some(&long), Some(&right))
            .unwrap()
            .as_bytes(),
        [7, 255, 255, 128]
    );
    assert_eq!(EnumLabelKey::between(None, None).unwrap().as_bytes(), [128]);
}

#[test]
fn invalid_keys_and_bounds_are_rejected() {
    assert_eq!(
        EnumLabelKey::from_bytes(Vec::new()),
        Err(EnumLabelKeyError::Empty)
    );
    assert_eq!(
        EnumLabelKey::from_bytes(vec![4, 0]),
        Err(EnumLabelKeyError::TrailingZero)
    );
    assert_eq!(
        EnumLabelKey::from_bytes(vec![1; MAX_ENUM_LABEL_KEY_BYTES + 1]),
        Err(EnumLabelKeyError::TooLong(MAX_ENUM_LABEL_KEY_BYTES + 1))
    );
    let key = EnumLabelKey::from_bytes(vec![9]).unwrap();
    assert_eq!(
        EnumLabelKey::between(Some(&key), Some(&key)),
        Err(EnumLabelKeyError::UnorderedBounds)
    );
    let full = EnumLabelKey::from_bytes(vec![255; MAX_ENUM_LABEL_KEY_BYTES]).unwrap();
    assert_eq!(
        EnumLabelKey::between(Some(&full), None),
        Err(EnumLabelKeyError::TooLong(MAX_ENUM_LABEL_KEY_BYTES + 1))
    );
}

#[test]
fn enum_values_order_by_type_then_label_key() {
    let keys = EnumLabelKey::initial(2).unwrap();
    let low = EnumValue::new(10, keys[0].clone());
    let high = EnumValue::new(10, keys[1].clone());
    let other = EnumValue::new(11, keys[0].clone());
    assert!(low < high);
    assert!(high < other);
    assert_eq!(low, EnumValue::new(10, keys[0].clone()));
    assert_eq!(low.retained_bytes(), 1);
}
