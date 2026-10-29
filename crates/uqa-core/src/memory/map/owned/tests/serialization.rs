//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

proptest! {
    #[test]
    fn owned_map_serialization_matches_ordered_map_values_and_collisions(
        entries in prop::collection::vec(("[a-z]{0,8}", any::<i64>()), 0..128)
    ) {
        let expected: BTreeMap<_, _> = entries.iter().cloned().collect();
        let owned: OwnedMap<_, _> = entries.into_iter().collect();
        let bytes = serde_json::to_vec(&owned).unwrap();
        prop_assert_eq!(&bytes, &serde_json::to_vec(&expected).unwrap());
        let decoded: OwnedMap<String, i64> = serde_json::from_slice(&bytes).unwrap();
        prop_assert!(decoded.iter().eq(expected.iter()));
    }
}

#[test]
fn owned_map_serialization_writes_borrowed_entries_without_scratch() {
    let owned: OwnedMap<_, _> = [("large", "x".repeat(1024 * 1024))].into_iter().collect();
    let expected = serde_json::to_vec(&owned).unwrap();
    let mut output = Vec::with_capacity(expected.len());
    let allocation = allocation_counter::measure(|| {
        serde_json::to_writer(&mut output, &owned).unwrap();
    });
    assert_eq!(output, expected);
    assert_eq!(allocation.count_total, 0, "{allocation:?}");
}

#[test]
fn owned_map_deserialization_keeps_last_values_and_releases_partial_errors() {
    let bytes = br#"{"z":[3],"a":[1],"z":[4,5]}"#;
    let owned: OwnedMap<String, Vec<u64>> = serde_json::from_slice(bytes).unwrap();
    let expected: BTreeMap<String, Vec<u64>> = serde_json::from_slice(bytes).unwrap();
    assert!(owned.iter().eq(expected.iter()));
    let allocation = allocation_counter::measure(|| {
        assert!(serde_json::from_slice::<OwnedMap<String, Vec<u64>>>(
            br#"{"a":[1,2],"z":[3,"invalid"]}"#
        )
        .is_err());
    });
    assert_eq!(allocation.bytes_current, 0, "{allocation:?}");
}
