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
    for (name, scalar, array) in [("int2vector", 22, 1006), ("oidvector", 30, 1013)] {
        assert_eq!(
            type_oid_in_schema(&catalog, "pg_catalog", name, 0),
            Some(scalar)
        );
        for dimensions in [1, 2] {
            assert_eq!(
                type_oid_in_schema(&catalog, "pg_catalog", name, dimensions),
                Some(array)
            );
            assert_eq!(
                type_oid_in_schema(&catalog, "pg_catalog", &format!("_{name}"), dimensions),
                Some(array)
            );
        }
    }
}
