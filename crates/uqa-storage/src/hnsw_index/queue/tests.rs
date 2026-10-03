//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn spilled_minimum_maximum_and_sorted_queues_preserve_total_float_order() {
    let control = StorageReadControl::with_limit(32 * 1024);
    let values = (0..80)
        .map(|node_id| Candidate {
            node_id,
            distance: f32::from_bits(
                [
                    0,
                    0x8000_0000,
                    0x3f80_0000,
                    0xbf80_0000,
                    0x7f80_0000,
                    0xff80_0000,
                    0x7fc0_0001,
                    0xffc0_0001,
                ][node_id as usize % 8],
            ),
        })
        .collect::<Vec<_>>();
    let mut expected = values.clone();
    expected.sort_unstable();
    let mut minimum = Queue::<true>::new(&control);
    let mut maximum = Queue::<false>::new(&control);
    let mut sorted = Queue::<false>::new(&control);
    for &candidate in values.iter().rev() {
        minimum.push(candidate).unwrap();
        maximum.push(candidate).unwrap();
        sorted.push(candidate).unwrap();
    }
    assert!(matches!(minimum.root, Root::Pages(_)));
    assert!(matches!(maximum.root, Root::Pages(_)));
    let sorted = sorted.into_sorted();
    assert_eq!(
        sorted
            .iter()
            .collect::<StorageBackendResult<Vec<_>>>()
            .unwrap(),
        expected
    );
    for (&first, &last) in expected.iter().zip(expected.iter().rev()) {
        assert_eq!(minimum.peek().unwrap(), Some(first));
        assert_eq!(minimum.pop().unwrap(), Some(first));
        assert_eq!(maximum.peek().unwrap(), Some(last));
        assert_eq!(maximum.pop().unwrap(), Some(last));
    }
    assert_eq!(minimum.pop().unwrap(), None);
    assert_eq!(maximum.pop().unwrap(), None);
    control.cancellation().cancel();
    assert!(minimum.push(values[0]).is_err());
    drop((minimum, maximum, sorted));
    assert_eq!(control.memory().used(), 0);
}
