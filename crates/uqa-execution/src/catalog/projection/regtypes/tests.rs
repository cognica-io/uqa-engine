//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn legacy_vector_array_type_lookup_uses_the_array_identity() {
    let mut catalog = RegtypeOutputCatalog {
        namespaces: BTreeMap::from([(11, "pg_catalog".into())]),
        classes: BTreeMap::new(),
        procs: BTreeMap::new(),
        proc_names_by_namespace: BTreeMap::new(),
        types: BTreeMap::new(),
        dependencies: std::sync::OnceLock::new(),
    };
    for (oid, name, array_oid, element_oid) in [
        (21, "int2", 1005, 0),
        (26, "oid", 1028, 0),
        (22, "int2vector", 1006, 21),
        (30, "oidvector", 1013, 26),
        (1006, "_int2vector", 0, 22),
        (1013, "_oidvector", 0, 30),
    ] {
        catalog.types.insert(
            oid,
            RegtypeCatalogEntry {
                name: name.into(),
                namespace_oid: 11,
                overloaded: false,
                argument_types: vec![],
                array_oid,
                element_oid,
            },
        );
    }
    let resolve = |name: &str, dimensions| {
        type_in_schema(&catalog, "pg_catalog", name)
            .and_then(|(oid, entry)| type_oid_for_dimensions(oid, entry, dimensions))
    };
    for (name, scalar, array) in [("int2vector", 22, 1006), ("oidvector", 30, 1013)] {
        assert_eq!(resolve(name, 0), Some(scalar));
        for dimensions in [1, 2] {
            assert_eq!(resolve(name, dimensions), Some(array));
            assert_eq!(resolve(&format!("_{name}"), dimensions), None);
        }
        assert_eq!(resolve(&format!("_{name}"), 0), Some(array));
    }
}

#[test]
fn type_output_qualifies_shadowed_rows_and_arrays_by_first_visible_type() {
    let mut catalog = RegtypeOutputCatalog {
        namespaces: BTreeMap::from([(20_000, "first".into()), (20_001, "second".into())]),
        classes: BTreeMap::new(),
        procs: BTreeMap::new(),
        proc_names_by_namespace: BTreeMap::new(),
        types: BTreeMap::new(),
        dependencies: std::sync::OnceLock::new(),
    };
    for (oid, namespace, name, array_oid, element_oid) in [
        (21_000, 20_000, "item", 21_001, 0),
        (21_001, 20_000, "_item", 0, 21_000),
        (22_000, 20_001, "item", 22_001, 0),
        (22_001, 20_001, "__item_1", 0, 22_000),
    ] {
        catalog.types.insert(
            oid,
            RegtypeCatalogEntry {
                name: name.into(),
                namespace_oid: namespace,
                overloaded: false,
                argument_types: vec![],
                array_oid,
                element_oid,
            },
        );
    }
    for (schemas, names) in [
        (
            ["first", "second"],
            ["item", "item[]", "second.item", "second.item[]"],
        ),
        (
            ["second", "first"],
            ["first.item", "first.item[]", "item", "item[]"],
        ),
    ] {
        let visibility = OutputVisibility {
            schemas: schemas.into_iter().map(str::to_owned).collect(),
        };
        for (oid, expected) in [21_000, 21_001, 22_000, 22_001].into_iter().zip(names) {
            assert_eq!(
                format_regtype(&visibility, &catalog, oid).as_deref(),
                Some(expected)
            );
        }
    }
}
