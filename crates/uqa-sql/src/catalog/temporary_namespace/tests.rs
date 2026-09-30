//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn namespace() -> TemporaryNamespace {
    TemporaryNamespace {
        schema: "pg_temp_7".into(),
        oids: TemporaryNamespaceOids {
            namespace: 16_390,
            toast_namespace: 16_391,
        },
    }
}

#[test]
fn the_toast_namespace_takes_the_temporary_namespace_number() {
    let namespace = namespace();
    assert_eq!(namespace.toast_schema(), "pg_toast_temp_7");
    assert_eq!(namespace.namespace_oid("pg_temp_7"), Some(16_390));
    assert_eq!(namespace.namespace_oid("pg_toast_temp_7"), Some(16_391));
    assert_eq!(namespace.namespace_oid("pg_temp_8"), None);
    assert_eq!(namespace.namespace_oid("pg_temp"), None);
    assert!(namespace.holds_oid(16_390));
    assert!(namespace.holds_oid(16_391));
    assert!(!namespace.holds_oid(16_392));
}

#[test]
fn temporary_schema_names_are_recognized_by_prefix() {
    assert!(is_temporary_schema_name("pg_temp_3"));
    assert!(is_temporary_schema_name("pg_toast_temp_3"));
    assert!(!is_temporary_schema_name("pg_temp"));
    assert!(!is_temporary_schema_name("pg_toast"));
    assert!(!is_temporary_schema_name("public"));
}
