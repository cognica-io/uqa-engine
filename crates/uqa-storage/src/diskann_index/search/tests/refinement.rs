//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn expanded_raw_distance_prevents_a_lossy_centroid_from_cutting_off_a_better_node() {
    // q=(1,0); centroid distances are 1/4, 5/4 and 4. The first raw
    // vector is (0,-1), at distance 2; the next is (3/5,4/5), at 4/5.
    // All three labels are nearest-centroid encodings of their raw vectors.
    let data = Oracle {
        query: vec![1.0, 0.0],
        centroids: vec![vec![0.5, 0.0], vec![0.0, 0.5], vec![-1.0, 0.0]],
        labels: vec![0, 1, 2],
        distances: vec![0.25, 1.25, 4.0],
        neighbors: vec![vec![1, 2], vec![2], vec![0]],
        entry: 0,
        cases: vec![],
        vectors: vec![vec![0.0, -1.0], vec![0.6, 0.8], vec![-1.0, 0.0]],
    };
    let case = Case {
        list: 1,
        beam: 1,
        rounds: vec![],
        expanded: vec![0, 1],
        completion: vec![2],
    };
    let physical = MemoryBudget::new(1 << 20);
    let (memory, manifest) = fixture_from(&data, 2, 3, &case, &physical);
    for cache in [0, 20 * PAGE_BYTES] {
        let owner = StorageReadControl::with_limit(65_536);
        let source = Arc::new(Source::new(memory.clone(), true));
        let reader = reader(source, &manifest, cache, &owner);
        let control = StorageReadControl::with_limit(65_536);
        let navigation = query(2, &control);
        check_lookup(&reader, &navigation, &data.distances, &control);
        for raw in &data.vectors {
            let NavigationInput::Navigable(vector) =
                NavigationInput::from_raw(2, raw, &control).unwrap()
            else {
                panic!("unit fixture vectors");
            };
            let expected = data.vectors.iter().position(|row| row == raw).unwrap();
            assert_eq!(
                &*reader
                    .codebook()
                    .unwrap()
                    .encode(&vector, &control)
                    .unwrap(),
                &[expected as u8]
            );
        }
        let mut traversal = DiskANNTraversal::new(reader, &navigation, &control).unwrap();
        drop(navigation);
        assert_eq!(ids(&traversal.next_beam().unwrap()), [0]);
        assert_eq!(traversal.workspace.as_ref().unwrap().frontier.len(), 1);
        assert_eq!(ids(&traversal.next_beam().unwrap()), [1]);
        assert!(traversal.next_beam().unwrap().is_empty());
        assert_eq!(traversal.stats().approximate_expansions, 2);
        assert_eq!(traversal.stats().completion_expansions, 0);
        assert_eq!(ids(&traversal.complete_next_beam().unwrap()), [2]);
        assert!(traversal.complete_next_beam().unwrap().is_empty());
        assert_eq!(control.memory().used(), 0);
    }
}
