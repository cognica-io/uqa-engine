//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::security::roles::persistence::RoleCatalogSnapshot;
use std::{collections::BTreeMap, sync::Arc};
use uqa_sql::SQLError;

fn snapshot() -> SequenceReadSnapshot {
    SequenceReadSnapshot {
        sequences: Arc::default(),
        object_ids: Arc::new(BTreeMap::from([(
            RelationIdentity::new("private", "ids"),
            [1; 16],
        )])),
        persistence: Arc::default(),
        security: Arc::default(),
        catalog_oids: Arc::new(BTreeMap::from([([1; 16], 16384)])),
        roles: RoleCatalogSnapshot {
            roles: Arc::default(),
            memberships: Arc::default(),
        },
    }
}

#[test]
fn sequence_oids_use_the_retained_identity_without_another_catalog_lookup() {
    assert_eq!(
        resolve_oid_reference(&snapshot(), 16384, |_| {
            panic!("a retained sequence OID must not resolve its name again")
        })
        .unwrap(),
        RelationIdentity::new("private", "ids")
    );
}

#[test]
fn other_relation_oids_preserve_the_actual_name_and_relation_kind() {
    for (relkind, kind) in [
        ("r", "table"),
        ("p", "partitioned table"),
        ("v", "view"),
        ("m", "materialized view"),
        ("i", "index"),
    ] {
        let error = resolve_oid_reference(&snapshot(), 16385, |oid| {
            assert_eq!(oid, 16385);
            Ok(Some(("not_a_sequence".into(), relkind.into())))
        })
        .unwrap_err();
        assert!(matches!(
            error,
            SequenceValueError::WrongKind { name, kind: actual }
                if name == "not_a_sequence" && actual == kind
        ));
    }
}

#[test]
fn absent_oids_and_catalog_failures_preserve_their_diagnostics() {
    let error = resolve_oid_reference(&snapshot(), 4_294_967_294, |_| Ok(None)).unwrap_err();
    assert!(matches!(
        error,
        SequenceValueError::MissingOid(4_294_967_294)
    ));
    let error = resolve_oid_reference(&snapshot(), 16385, |_| {
        Err(SQLError::Internal("catalog unavailable".into()))
    })
    .unwrap_err();
    assert!(matches!(
        error.into_sql_error(),
        SQLError::Internal(message) if message == "catalog unavailable"
    ));
}
