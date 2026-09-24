//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::catalog::roles::RoleDefinition;
use uqa_storage::SequenceSecurityRow;

fn empty() -> RestoredSequenceRegistry {
    RestoredSequenceRegistry {
        sequences: BTreeMap::new(),
        object_ids: BTreeMap::new(),
        persistence: BTreeMap::new(),
        security: BTreeMap::new(),
    }
}

fn roles() -> BTreeMap<String, RoleDefinition> {
    BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())])
}

#[test]
fn legacy_sequence_conversion_is_prepared_without_catalog_access_and_rejected_by_reads() {
    let mut legacy = row();
    legacy.security = SequenceSecurityRow::legacy("uqa");
    assert!(
        prepare_sequence_rows(empty(), vec![legacy.clone()], &roles())
            .err()
            .unwrap()
            .to_string()
            .contains("initial catalog migration")
    );
    let (registry, conversions) =
        prepare_sequence_rows_with_migration(empty(), vec![legacy.clone()], &roles(), true)
            .unwrap();
    assert_eq!(conversions.len(), 1);
    let mut expected = legacy.clone();
    expected.security = SequenceSecurityRow::bootstrap();
    assert_eq!(conversions[0], expected);
    assert_eq!(
        registry.security[&legacy.relation].role_owner,
        uqa_core::catalog_role::RoleIdentity::BOOTSTRAP
    );
    assert_eq!(
        registry.sequences[&legacy.relation],
        sequence_state_from_row(expected).unwrap().1
    );
    let (_, repeated) =
        prepare_sequence_rows_with_migration(empty(), conversions, &roles(), true).unwrap();
    assert!(repeated.is_empty());
}

#[test]
fn later_invalid_sequence_prevents_a_prepared_conversion_batch() {
    let mut legacy = row();
    legacy.security = SequenceSecurityRow::legacy("uqa");
    for defect in ["missing owner", "invalid definition", "duplicate object"] {
        let mut invalid = row();
        invalid.relation.name = "later".into();
        invalid.object_id = [9; 16];
        match defect {
            "missing owner" => invalid.security = SequenceSecurityRow::legacy("missing"),
            "invalid definition" => invalid.increment = 0,
            _ => invalid.object_id = legacy.object_id,
        }
        assert!(
            prepare_sequence_rows_with_migration(
                empty(),
                vec![legacy.clone(), invalid],
                &roles(),
                true
            )
            .is_err(),
            "{defect}"
        );
    }
}

#[test]
fn initial_migration_never_repairs_a_current_sequence_owner_from_a_reused_name_or_oid() {
    let current = row();
    let mut replacement = roles();
    replacement.get_mut("uqa").unwrap().object_id = [99; 16];
    for migration in [false, true] {
        let error = prepare_sequence_rows_with_migration(
            empty(),
            vec![current.clone()],
            &replacement,
            migration,
        )
        .err()
        .unwrap();
        assert!(
            error.to_string().contains("missing role incarnation"),
            "{error}"
        );
    }
}
