//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn a_whole_relation_gives_way_to_its_referenced_columns() {
    let mut references = References::default();
    references.add_relation(20_000);
    references.add_column(20_000, 3);
    references.add_type(16_385);
    references.add_column(20_000, 1);
    references.add_column(20_000, 3);
    references.add_type(16_385);
    assert_eq!(
        references.deduplicated(),
        [
            ObjectAddress::column(20_000, 1),
            ObjectAddress::column(20_000, 3),
            ObjectAddress::whole(TYPE_CLASS, 16_385),
        ]
    );
}

#[test]
fn a_relation_without_referenced_columns_stays_whole() {
    let mut references = References::default();
    references.add_relation(20_000);
    references.add_routine(20_000);
    references.add_relation(20_000);
    assert_eq!(
        references.deduplicated(),
        [
            ObjectAddress::whole(PROCEDURE_CLASS, 20_000),
            ObjectAddress::whole(RELATION_CLASS, 20_000),
        ]
    );
}
