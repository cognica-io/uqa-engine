//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    read_sequence, sequence_relation_oid, BTreeMap, RelationIdentity, RelationPersistence,
    RoleCatalogSnapshot, SequenceDataType, SequenceSecurity, SequenceSnapshotSource, SequenceState,
    StorageBackendResult,
};
use crate::catalog::sequence::snapshot::SequenceReadSnapshot;
use std::{
    cell::{Cell, RefCell},
    sync::Arc,
};
use uqa_sql::catalog::roles::RoleDefinition;

struct TestCatalog {
    snapshot: RefCell<SequenceReadSnapshot>,
    reads: Cell<usize>,
}

impl SequenceSnapshotSource for TestCatalog {
    fn sequence_read_snapshot(&self) -> StorageBackendResult<SequenceReadSnapshot> {
        self.reads.set(self.reads.get() + 1);
        Ok(self.snapshot.borrow().clone())
    }
}

#[test]
fn sequence_introspection_retains_metadata_and_roles_together_and_rejects_missing_state() {
    let relation = RelationIdentity::from_legacy_name("public.counter").unwrap();
    let object_id = [7; 16];
    let state = SequenceState::initial(1, 1, SequenceDataType::BigInt);
    let roles = BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())]);
    let security = SequenceSecurity {
        role_owner: "uqa".into(),
        acl: None,
    };
    let catalog = TestCatalog {
        snapshot: RefCell::new(SequenceReadSnapshot {
            sequences: Arc::new(BTreeMap::from([(relation.clone(), state)])),
            object_ids: Arc::new(BTreeMap::from([(relation.clone(), object_id)])),
            persistence: Arc::new(BTreeMap::from([(
                relation.clone(),
                RelationPersistence::Unlogged,
            )])),
            security: Arc::new(BTreeMap::from([(
                relation.clone(),
                uqa_sql::catalog::security::BoundSequenceSecurity::bind(&security, &roles).unwrap(),
            )])),
            roles: RoleCatalogSnapshot {
                roles: Arc::new(roles),
                memberships: Arc::new(BTreeMap::new()),
            },
        }),
        reads: Cell::new(0),
    };
    let sequence = read_sequence(&catalog, sequence_relation_oid(object_id))
        .unwrap()
        .unwrap();
    assert_eq!(catalog.reads.get(), 1);
    {
        let mut current = catalog.snapshot.borrow_mut();
        Arc::make_mut(&mut current.sequences).clear();
        Arc::make_mut(&mut current.security).clear();
        Arc::make_mut(&mut current.roles.roles).clear();
    }
    assert_eq!(sequence.relation, relation);
    assert_eq!(sequence.state, state);
    assert_eq!(sequence.security, security);
    assert_eq!(sequence.persistence, RelationPersistence::Unlogged);
    assert!(sequence.authority.roles.contains_key("uqa"));
    let error = read_sequence(&catalog, sequence_relation_oid(object_id))
        .err()
        .unwrap();
    assert!(error.to_string().contains("disappeared"), "{error}");
    assert_eq!(catalog.reads.get(), 2);
    assert!(read_sequence(&catalog, sequence_relation_oid([8; 16]))
        .unwrap()
        .is_none());
}
