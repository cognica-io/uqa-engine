//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::oids::stable_object_oid;

#[test]
fn legacy_relations_keep_the_oids_their_identity_derived() {
    let object = [7; 16];
    let derive = |label| u32::try_from(stable_object_oid(label, &object)).unwrap();
    let table = RelationCatalogOids::legacy(RelationOidKind::Table, &object);
    assert_eq!(
        table,
        RelationCatalogOids {
            relation: derive("relation"),
            row_type: Some(derive("rowtype")),
            array_type: None,
            rule: None,
        }
    );
    let view = RelationCatalogOids::legacy(RelationOidKind::View, &object);
    assert_eq!(view.rule, Some(derive("view-rule")));
    let sequence = RelationCatalogOids::legacy(RelationOidKind::Sequence, &object);
    assert_eq!((sequence.row_type, sequence.reltype()), (None, 0));
}

#[test]
fn recorded_oids_carry_exactly_the_companions_of_their_kind() {
    let table = RelationCatalogOids {
        relation: 16_384,
        row_type: Some(16_386),
        array_type: Some(16_385),
        rule: None,
    };
    assert!(table.is_valid_for(RelationOidKind::Table, &[7; 16]));
    assert!(table.is_valid_for(RelationOidKind::ForeignTable, &[7; 16]));
    assert!(!table.is_valid_for(RelationOidKind::View, &[7; 16]));
    assert!(!table.is_valid_for(RelationOidKind::Sequence, &[7; 16]));
    assert_eq!(
        table.claimed().collect::<Vec<_>>(),
        [16_384, 16_386, 16_385]
    );
    let sequence = RelationCatalogOids {
        relation: 16_390,
        row_type: None,
        array_type: None,
        rule: None,
    };
    assert!(sequence.is_valid_for(RelationOidKind::Sequence, &[7; 16]));
    let system = RelationCatalogOids {
        relation: 1259,
        ..sequence
    };
    assert!(!system.is_valid_for(RelationOidKind::Sequence, &[7; 16]));
}

#[test]
fn low_legacy_oids_require_exact_original_companions_and_a_normal_array_oid() {
    for (kind, identity, low_oid) in [
        (RelationOidKind::Table, 248_721_u128, 10_274),
        (RelationOidKind::ForeignTable, 44_683, 10_938),
        (RelationOidKind::View, 2_270, 11_015),
        (RelationOidKind::Sequence, 248_721, 10_274),
        (RelationOidKind::CompositeType, 44_683, 10_938),
    ] {
        let object_id = identity.to_le_bytes();
        let mut recorded = RelationCatalogOids::legacy(kind, &object_id);
        assert!(recorded.claimed().any(|oid| oid == low_oid));
        let has_array = kind != RelationOidKind::Sequence;
        recorded.array_type = has_array.then_some(16_384);
        assert!(recorded.is_valid_for(kind, &object_id), "{kind:?}");
        assert!(!recorded.is_valid_for(kind, &[7; 16]), "{kind:?}");

        let mut replaced = recorded;
        replaced.relation += 1;
        assert!(!replaced.is_valid_for(kind, &object_id), "{kind:?}");
        if has_array {
            replaced = recorded;
            replaced.array_type = Some(16_383);
            assert!(!replaced.is_valid_for(kind, &object_id), "{kind:?}");
            replaced.array_type = None;
            assert!(!replaced.is_valid_for(kind, &object_id), "{kind:?}");
        }
    }
}
