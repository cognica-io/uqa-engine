//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private definitions are selected as complete immutable records.

use super::*;
use uqa_sql::{ast::SequenceDataType, catalog::roles::RoleDefinition};

fn registry(entries: &[(&str, u8, i64)]) -> SequenceReadSnapshot {
    let mut snapshot = SequenceReadSnapshot {
        sequences: Arc::new(BTreeMap::new()),
        object_ids: Arc::new(BTreeMap::new()),
        persistence: Arc::new(BTreeMap::new()),
        security: Arc::new(BTreeMap::new()),
        roles: RoleCatalogSnapshot {
            roles: Arc::new(BTreeMap::from([(
                "uqa".into(),
                RoleDefinition::bootstrap(),
            )])),
            memberships: Arc::new(BTreeMap::new()),
        },
    };
    for &(name, identity, start) in entries {
        let relation = RelationIdentity::new("public", name);
        let mut state = SequenceState::initial(start, 1, SequenceDataType::BigInt);
        state.definition_generation = [identity; 16];
        Arc::make_mut(&mut snapshot.sequences).insert(relation.clone(), state);
        Arc::make_mut(&mut snapshot.object_ids).insert(relation.clone(), [identity; 16]);
        Arc::make_mut(&mut snapshot.persistence)
            .insert(relation.clone(), RelationPersistence::Permanent);
        Arc::make_mut(&mut snapshot.security).insert(
            relation,
            SequenceSecurity {
                role_owner: "uqa".into(),
                acl: None,
            },
        );
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
    Arc::make_mut(&mut current.security)
        .get_mut(&renamed)
        .unwrap()
        .acl = Some(Vec::new());
    let temporary = RelationIdentity::new("public", "temporary");
    Arc::make_mut(&mut current.persistence)
        .insert(temporary.clone(), RelationPersistence::Temporary);
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
            .sequences
            .iter()
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
    let names = merged.sequences.keys().collect::<Vec<_>>();
    assert_eq!(merged.object_ids.keys().collect::<Vec<_>>(), names);
    assert_eq!(merged.persistence.keys().collect::<Vec<_>>(), names);
    assert_eq!(merged.security.keys().collect::<Vec<_>>(), names);
    assert_eq!(merged.object_ids[&renamed], [1; 16]);
    assert_eq!(merged.security[&renamed].acl, Some(Vec::new()));
    assert_eq!(
        merged.persistence[&temporary],
        RelationPersistence::Temporary
    );
    assert!(retained
        .sequences
        .contains_key(&RelationIdentity::new("public", "old_name")));
    assert!(!retained.sequences.contains_key(&renamed));
    assert_eq!(
        current.sequences[&RelationIdentity::new("public", "shared")].start,
        40
    );
}

#[test]
fn private_definition_merge_rejects_incomplete_records_and_failed_revision_reads() {
    let current = registry(&[("created", 1, 10)]);
    let committed = registry(&[("published", 2, 20)]);
    let retained = committed.clone();
    let mut corrupt = current.clone();
    Arc::make_mut(&mut corrupt.security).clear();
    let error = committed
        .clone()
        .merge_sequence_records(&corrupt, |_, _| Ok(true))
        .err()
        .unwrap();
    assert!(error
        .to_string()
        .contains("incomplete private catalog metadata"));
    let error = committed
        .merge_sequence_records(&current, |_, _| {
            Err(StorageBackendError::Other(
                "private view unavailable".into(),
            ))
        })
        .err()
        .unwrap();
    assert_eq!(error.to_string(), "private view unavailable");
    assert_eq!(retained.sequences.len(), 1);
    assert!(retained
        .sequences
        .contains_key(&RelationIdentity::new("public", "published")));
    assert_eq!(current.security.len(), 1);
}
