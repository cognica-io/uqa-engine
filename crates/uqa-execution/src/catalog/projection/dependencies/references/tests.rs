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

#[test]
fn constant_dependencies_use_catalog_alias_identities_including_quoted_names() {
    use uqa_sql::catalog::dependencies::{COLLATION_CLASS, NAMESPACE_CLASS};
    use uqa_sql::ColumnType;

    for (ty, class) in [
        (ColumnType::Regclass, RELATION_CLASS),
        (ColumnType::Regtype, TYPE_CLASS),
        (ColumnType::Regproc, PROCEDURE_CLASS),
        (ColumnType::Regprocedure, PROCEDURE_CLASS),
        (ColumnType::Regnamespace, NAMESPACE_CLASS),
        (ColumnType::Regcollation, COLLATION_CLASS),
    ] {
        for name in [ty.sql_name(), ty.catalog_name()] {
            let mut references = References::default();
            super::super::expressions::add_constant_reference(&name, 20_000, &mut references);
            assert_eq!(
                references.deduplicated(),
                [ObjectAddress::whole(class, 20_000)],
                "{name}",
            );
        }
    }
    for name in [
        "pg_catalog.\"REGCLASS\"",
        "domain#50001",
        "pg_catalog.integer",
    ] {
        let mut references = References::default();
        super::super::expressions::add_constant_reference(name, 20_000, &mut references);
        assert!(references.is_empty(), "{name}");
    }
}
