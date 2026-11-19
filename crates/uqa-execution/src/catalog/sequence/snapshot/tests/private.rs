//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private definitions are selected as complete immutable records.

use super::*;
use crate::catalog::sequence::registry::SequenceRecord;
use uqa_sql::catalog::roles::{RoleDefinitions, RoleMemberships};
use uqa_sql::catalog::security::BoundSequenceSecurity;
use uqa_sql::{ast::SequenceDataType, catalog::roles::RoleDefinition};

fn registry(entries: &[(&str, u8, i64)]) -> SequenceReadSnapshot {
    let mut snapshot = SequenceReadSnapshot {
        catalog: uqa_core::memory::RetainedValue::new(SequenceCatalog::with_budget(
            &uqa_core::memory::MemoryBudget::new(1 << 20),
        )),
        roles: RoleCatalogSnapshot {
            roles: uqa_core::memory::RetainedValue::new(
                RoleDefinitions::try_from_iter([("uqa".into(), RoleDefinition::bootstrap())])
                    .unwrap(),
            ),
            memberships: uqa_core::memory::RetainedValue::new(RoleMemberships::new()),
        },
    };
    for &(name, identity, start) in entries {
        let relation = RelationIdentity::new("public", name);
        let mut state = SequenceState::initial(start, 1, SequenceDataType::BigInt);
        state.definition_generation = [identity; 16];
        uqa_core::memory::RetainedValue::make_mut(&mut snapshot.catalog)
            .insert(
                relation,
                SequenceRecord {
                    state,
                    object_id: [identity; 16],
                    persistence: RelationPersistence::Permanent,
                    catalog_oid: None,
                },
                BoundSequenceSecurity::owner(uqa_core::catalog_role::RoleIdentity::BOOTSTRAP),
            )
            .unwrap();
    }
    snapshot
}

#[test]
fn private_definition_merge_retains_renames_deletions_security_and_temporary_records() {
    let mut current = registry(&[
        ("renamed", 1, 11),
        ("created", 3, 31),
        ("shared", 4, 40),
        ("temporary", 5, 51),
    ]);
    let renamed = RelationIdentity::new("public", "renamed");
    let mut security = current.catalog.security()[&renamed].clone();
    security.acl = Some(Vec::new());
    uqa_core::memory::RetainedValue::make_mut(&mut current.catalog)
        .replace_security(&renamed, security)
        .unwrap();
    let temporary = RelationIdentity::new("public", "temporary");
    let record = SequenceRecord {
        persistence: RelationPersistence::Temporary,
        ..*current.catalog.get(&temporary).unwrap()
    };
    let security = current.catalog.security()[&temporary].clone();
    uqa_core::memory::RetainedValue::make_mut(&mut current.catalog)
        .insert(temporary.clone(), record, security)
        .unwrap();
    let committed = registry(&[
        ("old_name", 1, 10),
        ("deleted", 2, 20),
        ("shared", 4, 400),
        ("published", 6, 60),
    ]);
    let retained = committed.clone();
    let merged = committed
        .merge_sequence_records(&current, |relation, object_id| {
            assert_ne!(
                relation, &temporary,
                "temporary entries never query durable revisions"
            );
            Ok(matches!(object_id[0], 1..=3))
        })
        .unwrap();
    assert_eq!(
        merged
            .catalog
            .states()
            .map(|(name, state)| (name.name.as_str(), state.start))
            .collect::<Vec<_>>(),
        [
            ("created", 31),
            ("published", 60),
            ("renamed", 11),
            ("shared", 400),
            ("temporary", 51)
        ]
    );
    let names = merged.catalog.keys().collect::<Vec<_>>();
    assert_eq!(
        merged
            .catalog
            .object_ids()
            .map(|(key, _)| key)
            .collect::<Vec<_>>(),
        names
    );
    assert_eq!(
        merged
            .catalog
            .persistences()
            .map(|(key, _)| key)
            .collect::<Vec<_>>(),
        names
    );
    assert_eq!(
        merged
            .catalog
            .security()
            .iter()
            .map(|(key, _)| key)
            .collect::<Vec<_>>(),
        names
    );
    assert_eq!(merged.catalog.object_id(&renamed), Some(&[1; 16]));
    assert_eq!(merged.catalog.security()[&renamed].acl, Some(Vec::new()));
    assert_eq!(
        merged.catalog.persistence(&temporary),
        Some(RelationPersistence::Temporary)
    );
    assert!(retained
        .catalog
        .contains_key(&RelationIdentity::new("public", "old_name")));
    assert!(!retained.catalog.contains_key(&renamed));
    assert_eq!(
        current
            .catalog
            .state(&RelationIdentity::new("public", "shared"))
            .unwrap()
            .start,
        40
    );
}

#[test]
fn private_definition_merge_retains_inputs_after_failed_admission_and_revision_reads() {
    let current = registry(&[("created", 1, 10)]);
    let committed = registry(&[("published", 2, 20)]);
    let retained = committed.clone();
    let memory = committed.catalog.budget().clone();
    let used = memory.used();
    let occupied = memory.reserve(memory.available()).unwrap();
    assert!(committed
        .clone()
        .merge_sequence_records(&current, |_, _| Ok(true))
        .is_err());
    drop(occupied);
    assert_eq!(memory.used(), used);
    let error = committed
        .merge_sequence_records(&current, |_, _| {
            Err(StorageBackendError::Other(
                "private view unavailable".into(),
            ))
        })
        .err()
        .unwrap();
    assert_eq!(error.to_string(), "private view unavailable");
    assert_eq!(retained.catalog.len(), 1);
    assert!(retained
        .catalog
        .contains_key(&RelationIdentity::new("public", "published")));
    assert_eq!(current.catalog.security().len(), 1);
}

#[test]
fn detached_sequence_inspection_preserves_candidate_order_and_retained_names() {
    let mut current = registry(&[("alpha", 1, 10), ("beta", 2, 20)]);
    let retained = current.clone();
    let alpha = RelationIdentity::new("public", "alpha");
    let beta = RelationIdentity::new("public", "beta");
    let missing = RelationIdentity::new("public", "missing");
    uqa_core::memory::RetainedValue::make_mut(&mut current.catalog)
        .remove(&alpha)
        .unwrap();
    assert_eq!(retained.names(), ["public.alpha", "public.beta"]);
    assert_eq!(retained.named_states()["public.alpha"].start, 10);
    assert_eq!(
        retained.first_state(&[missing.clone(), beta.clone(), alpha]),
        Some((
            "public.beta".into(),
            *retained.catalog.state(&beta).unwrap()
        ))
    );
    assert!(retained.first_state(&[missing]).is_none());
    assert_eq!(current.names(), ["public.beta"]);
}
