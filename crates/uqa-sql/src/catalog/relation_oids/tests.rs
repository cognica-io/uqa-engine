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
    assert!(table.is_valid_for(RelationOidKind::Table));
    assert!(table.is_valid_for(RelationOidKind::ForeignTable));
    assert!(!table.is_valid_for(RelationOidKind::View));
    assert!(!table.is_valid_for(RelationOidKind::Sequence));
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
    assert!(sequence.is_valid_for(RelationOidKind::Sequence));
    let system = RelationCatalogOids {
        relation: 1259,
        ..sequence
    };
    assert!(!system.is_valid_for(RelationOidKind::Sequence));
}
