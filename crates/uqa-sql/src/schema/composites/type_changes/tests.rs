//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::dependencies::{Dependency, DependencyKind};

fn edge(dependent: ObjectAddress, referenced: ObjectAddress) -> Dependency {
    Dependency {
        dependent,
        referenced,
        kind: DependencyKind::Normal,
    }
}

#[test]
fn storage_dependency_walk_follows_containers_in_order_and_terminates_cycles() {
    let original = ObjectAddress::whole(TYPE_CLASS, 20_001);
    let array = ObjectAddress::whole(TYPE_CLASS, 20_002);
    let nested = ObjectAddress::whole(TYPE_CLASS, 20_003);
    let virtual_column = ObjectAddress::column(20_004, 1);
    let stored_column = ObjectAddress::column(20_005, 2);
    let later_column = ObjectAddress::column(20_006, 1);
    let graph = DependencyGraph::new([
        edge(array, original),
        edge(virtual_column, array),
        edge(original, nested),
        edge(stored_column, nested),
        edge(later_column, original),
    ]);
    let visited = std::cell::RefCell::new(Vec::new());
    let error = reject_stored_uses(&graph, 20_001, "pair", |address, target| {
        visited.borrow_mut().push((address, target));
        match address {
            address if address == virtual_column => Some(CompositeStorageUse::RowType(20_003)),
            address if address == stored_column => Some(CompositeStorageUse::Stored {
                relation: "holder",
                column: "items",
            }),
            _ => panic!("a later dependent must not precede the first stored use"),
        }
    })
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("0A000"));
    assert_eq!(
        error.to_string(),
        "cannot alter type \"pair\" because column \"holder.items\" uses it"
    );
    assert_eq!(
        *visited.borrow(),
        [(virtual_column, 20_002), (stored_column, 20_003)]
    );
    let cycle = DependencyGraph::new([edge(array, original), edge(original, array)]);
    reject_stored_uses(&cycle, 20_001, "pair", |_, _| {
        panic!("no relation dependency")
    })
    .unwrap();
}

#[test]
fn field_dependency_diagnostics_use_postgresql_default_and_domain_rules() {
    let attribute = ObjectAddress::column(20_001, 2);
    for (class, expected) in [
        (ATTRIBUTE_DEFAULT_CLASS, "0A000"),
        (CONSTRAINT_CLASS, "XX000"),
    ] {
        let dependent = ObjectAddress::whole(class, 20_002);
        let graph = DependencyGraph::new([edge(dependent, attribute)]);
        let error = reject_field_dependents(
            &graph,
            attribute,
            "a",
            |_| panic!("no object description"),
            |_| {
                if class == ATTRIBUTE_DEFAULT_CLASS {
                    FieldDependent::ColumnDefault("v".into())
                } else {
                    FieldDependent::DomainConstraint
                }
            },
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some(expected));
        if class == ATTRIBUTE_DEFAULT_CLASS {
            let SQLError::Diagnostic {
                message, detail, ..
            } = error
            else {
                panic!("default detail")
            };
            assert_eq!(
                message,
                "cannot alter type of a column used by a generated column"
            );
            assert_eq!(
                detail.as_deref(),
                Some("Column \"a\" is used by generated column \"v\".")
            );
        } else {
            assert_eq!(
                error.to_string(),
                "could not identify relation associated with constraint 20002"
            );
        }
    }
}
