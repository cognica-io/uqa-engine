//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Grouping-set, rollup, cube, and grouping-distinct compilation.

use super::*;

#[test]
fn explicit_grouping_sets_preserve_every_key_expression() {
    let Statement::Select(select) =
        first("SELECT g, v, count(*) FROM spill_data GROUP BY GROUPING SETS ((g), (v), ())")
    else {
        panic!("not SELECT");
    };
    assert_eq!(
        select.grouping_sets.len(),
        3,
        "compiled grouping sets: {:?}",
        select.grouping_sets
    );
    assert_eq!(select.grouping_sets[0].len(), 1);
    assert_eq!(select.grouping_sets[1].len(), 1);
    assert!(select.grouping_sets[2].is_empty());
}

#[test]
fn rollup_cube_and_multiple_grouping_items_expand_without_dropping_keys() {
    let Statement::Select(rollup) = first("SELECT g, v, count(*) FROM t GROUP BY ROLLUP (g, v)")
    else {
        panic!("not SELECT");
    };
    assert_eq!(
        rollup
            .grouping_sets
            .iter()
            .map(Vec::len)
            .collect::<Vec<_>>(),
        vec![2, 1, 0]
    );

    let Statement::Select(cube) = first("SELECT g, v, count(*) FROM t GROUP BY CUBE (g, v)") else {
        panic!("not SELECT");
    };
    let mut cube_widths = cube.grouping_sets.iter().map(Vec::len).collect::<Vec<_>>();
    cube_widths.sort_unstable();
    assert_eq!(cube_widths, vec![0, 1, 1, 2]);

    let Statement::Select(product) = first(
        "SELECT a, b, c, d, count(*) FROM t \
         GROUP BY GROUPING SETS ((a), (b)), GROUPING SETS ((c), (d))",
    ) else {
        panic!("not SELECT");
    };
    assert_eq!(product.grouping_sets.len(), 4);
    assert!(product.grouping_sets.iter().all(|set| set.len() == 2));
}

#[test]
fn group_by_distinct_is_preserved_for_post_binding_deduplication() {
    let Statement::Select(plain) = first("SELECT g, count(*) FROM t GROUP BY DISTINCT g") else {
        panic!("not SELECT");
    };
    assert!(plain.group_distinct);
    assert_eq!(plain.group_by.len(), 1);
    assert!(plain.grouping_sets.is_empty());

    let Statement::Select(repeated) = first(
        "SELECT g, v, count(*) FROM t \
         GROUP BY DISTINCT GROUPING SETS ((g), (g), (v), (g))",
    ) else {
        panic!("not SELECT");
    };
    let Statement::Select(all) = first(
        "SELECT g, v, count(*) FROM t \
         GROUP BY ALL GROUPING SETS ((g), (g), (v), (g))",
    ) else {
        panic!("not SELECT");
    };
    assert!(repeated.group_distinct);
    assert!(!all.group_distinct);
    assert_eq!(
        repeated.grouping_sets.len(),
        4,
        "the compiler must retain duplicates until input types are bound"
    );
    assert_eq!(
        serde_json::to_value(&repeated.grouping_sets).unwrap(),
        serde_json::to_value(&all.grouping_sets).unwrap()
    );

    let Statement::Select(alias) = first(
        "SELECT g + 1 AS shifted, count(*) FROM t \
         GROUP BY DISTINCT GROUPING SETS ((shifted), (g + 1))",
    ) else {
        panic!("not SELECT");
    };
    assert_eq!(alias.grouping_sets.len(), 2);
    assert_eq!(
        serde_json::to_value(&alias.grouping_sets[0]).unwrap(),
        serde_json::to_value(&alias.grouping_sets[1]).unwrap(),
        "alias resolution precedes type-aware grouping-set deduplication"
    );

    let Statement::Select(explicit_rows) = first(
        "SELECT count(*) FROM t \
         GROUP BY DISTINCT GROUPING SETS ((ROW(g, v)), (ROW(v, g)))",
    ) else {
        panic!("not SELECT");
    };
    assert!(explicit_rows.group_distinct);
    assert_eq!(explicit_rows.grouping_sets.len(), 2);
    assert!(explicit_rows
        .grouping_sets
        .iter()
        .all(|set| matches!(set.as_slice(), [Expr::Row(_)])));
}
