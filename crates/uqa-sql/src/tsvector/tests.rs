//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn repeated_lexemes_keep_the_first_sorted_weight_at_the_maximum_position() {
    let control = ProductionControl::uncontrolled();
    for (input, weight) in [("a:16383D a:1 a:16383A", 0), ("a:16383A a:1 a:16383D", 3)] {
        let words = parse(input, &control).unwrap();
        assert_eq!(words.len(), 1);
        assert_eq!(words[0].positions, [(1, 0), (16383, weight)]);
    }
}

#[test]
fn the_256th_distinct_position_keeps_its_postgresql_sorted_weight() {
    let descending = (1..=256)
        .rev()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let input = format!("a:256D,256A,{descending}");
    let words = parse(&input, &ProductionControl::uncontrolled()).unwrap();
    assert_eq!(words[0].positions.len(), 256);
    assert_eq!(words[0].positions.last(), Some(&(256, 3)));
}
