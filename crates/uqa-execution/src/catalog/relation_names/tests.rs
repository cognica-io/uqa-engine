//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Name binding needs point lookups, not an inventory of table definitions.

use super::*;
use std::{cell::Cell, collections::BTreeSet};

struct TableNames {
    names: BTreeSet<RelationIdentity>,
    reads: Cell<usize>,
}

impl RelationTableNames for TableNames {
    fn contains_table(&self, relation: &RelationIdentity) -> bool {
        self.reads.set(self.reads.get() + 1);
        self.names.contains(relation)
    }
}

#[test]
fn bound_relation_kinds_need_one_table_probe_regardless_of_catalog_size() {
    let snapshot = crate::catalog::test_support::empty_catalog();
    let resolution = RelationNameResolution {
        search_path: vec!["unrelated".into()],
        temporary_schema: "pg_temp_1".into(),
        temporary_namespace_allocated: false,
        current_user: "uqa".into(),
        lookup_mode: RelationLookupMode::Bound,
    };
    for fillers in [0, 60] {
        let mut names = BTreeSet::from([RelationIdentity::new("public", "items")]);
        names.extend(
            (0..fillers).map(|index| RelationIdentity::new("public", format!("filler_{index}"))),
        );
        let names = TableNames {
            names,
            reads: Cell::new(0),
        };
        let catalog = RelationNameCatalog::new(&names, &snapshot.snapshot().definitions);
        assert_eq!(
            catalog
                .relation_kind_resolution(&resolution, "public.items")
                .unwrap(),
            RelationResolution::Found("public.items".into(), "table")
        );
        assert_eq!(names.reads.get(), 1);
        assert_eq!(
            catalog
                .relation_kind_resolution(&resolution, "public.absent")
                .unwrap(),
            RelationResolution::MissingRelation
        );
        assert_eq!(names.reads.get(), 2);
        assert_eq!(
            catalog
                .relation_kind_resolution(&resolution, "pg_catalog.pg_class")
                .unwrap(),
            RelationResolution::Found("pg_catalog.pg_class".into(), "table")
        );
        assert!(catalog
            .relation_kind_resolution(&resolution, "items")
            .is_err());
        assert_eq!(
            names.reads.get(),
            2,
            "virtual and invalid bindings do not read table names"
        );
    }
}
