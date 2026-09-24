//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::TableConstraintSet;

struct Names(Vec<RelationIdentity>);

impl StoredTableNames for Names {
    fn stored_table_exists(&self, relation: &RelationIdentity) -> bool {
        self.0.contains(relation)
    }

    fn stored_table_names(&self) -> Vec<RelationIdentity> {
        self.0.clone()
    }
}

#[test]
fn legacy_declaration_binding_covers_columns_tables_and_inherited_references() {
    let crate::Statement::CreateTable(table) = crate::compile("CREATE TABLE child(a int REFERENCES parent(k), b int, FOREIGN KEY(b) REFERENCES parent(k))").unwrap().remove(0) else { panic!("table declaration") };
    let mut columns = table.columns;
    let mut constraints = TableConstraintSet {
        foreign_keys: table.foreign_keys,
        ..Default::default()
    };
    constraints.hierarchy.partition_inherited_foreign_keys = constraints.foreign_keys.clone();
    let names = Names(vec![RelationIdentity::new("app", "parent")]);
    assert!(bind_stored_foreign_key_declarations(&names, &mut columns, &mut constraints).unwrap());
    assert_eq!(columns[0].references.as_ref().unwrap().table, "app.parent");
    assert_eq!(constraints.foreign_keys[0].ref_table, "app.parent");
    assert_eq!(
        constraints.hierarchy.partition_inherited_foreign_keys[0].ref_table,
        "app.parent"
    );
    assert!(!bind_stored_foreign_key_declarations(&names, &mut columns, &mut constraints).unwrap());
}

#[test]
fn legacy_target_resolution_requires_exactly_one_stored_relation() {
    let mut names = Names(vec![RelationIdentity::new("app", "parent")]);
    assert_eq!(
        canonical_stored_foreign_key_target(&names, "parent").unwrap(),
        "app.parent"
    );
    names.0.push(RelationIdentity::new("public", "parent"));
    assert!(canonical_stored_foreign_key_target(&names, "parent")
        .unwrap_err()
        .contains("ambiguous persisted"));
    assert_eq!(
        canonical_stored_foreign_key_target(&names, "app.parent").unwrap(),
        "app.parent"
    );
    for missing in ["missing", "missing.parent"] {
        assert!(canonical_stored_foreign_key_target(&names, missing)
            .unwrap_err()
            .contains("dangling persisted"));
    }
}
