//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::security::roles::persistence::RoleCatalogSnapshot;
use crate::catalog::sequence::{
    registry::{SequenceCatalog, SequenceRecord},
    SequenceState,
};
use uqa_sql::SQLError;

fn snapshot() -> SequenceReadSnapshot {
    SequenceReadSnapshot {
        catalog: uqa_core::memory::RetainedValue::new(
            SequenceCatalog::try_from_iter([(
                RelationIdentity::new("private", "ids"),
                SequenceRecord {
                    state: SequenceState::initial(1, 1, uqa_sql::ast::SequenceDataType::BigInt),
                    object_id: [1; 16],
                    persistence: uqa_sql::ast::RelationPersistence::Permanent,
                    catalog_oid: Some(16384),
                },
                uqa_sql::catalog::security::BoundSequenceSecurity::owner(
                    uqa_core::catalog_role::RoleIdentity::BOOTSTRAP,
                ),
            )])
            .unwrap(),
        ),
        roles: RoleCatalogSnapshot {
            roles: uqa_core::memory::RetainedValue::default(),
            memberships: uqa_core::memory::RetainedValue::default(),
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
