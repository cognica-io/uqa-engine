//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    read_sequence, sequence_relation_oid, BTreeMap, RelationIdentity, RelationPersistence,
    SequenceDataType, SequenceIntrospectionCatalog, SequenceObjectIdsRead, SequenceSecurity,
    SequenceState, SequenceStatesRead, StorageBackendResult,
};
use std::{
    cell::{Cell, RefCell},
    ops::Deref,
};

struct TestCatalog {
    ids: BTreeMap<RelationIdentity, [u8; 16]>,
    states: BTreeMap<RelationIdentity, SequenceState>,
    held: Cell<bool>,
    events: RefCell<Vec<&'static str>>,
}

struct ObjectIdsGuard<'a>(&'a TestCatalog);

impl Deref for ObjectIdsGuard<'_> {
    type Target = BTreeMap<RelationIdentity, [u8; 16]>;
    fn deref(&self) -> &Self::Target {
        &self.0.ids
    }
}
impl Drop for ObjectIdsGuard<'_> {
    fn drop(&mut self) {
        assert!(self.0.held.replace(false));
        self.0.events.borrow_mut().push("release");
    }
}
impl SequenceIntrospectionCatalog for TestCatalog {
    fn refresh_sequences(&self) -> StorageBackendResult<()> {
        assert!(!self.held.get());
        self.events.borrow_mut().push("refresh");
        Ok(())
    }
    fn object_ids(&self) -> SequenceObjectIdsRead<'_> {
        assert!(!self.held.replace(true));
        self.events.borrow_mut().push("object_ids");
        Box::new(ObjectIdsGuard(self))
    }
    fn states(&self) -> SequenceStatesRead<'_> {
        Box::new(&self.states)
    }
    fn sequence_state(&self, relation: &RelationIdentity) -> Option<SequenceState> {
        assert!(self.held.get());
        self.events.borrow_mut().push("state");
        self.states.get(relation).copied()
    }
    fn sequence_security(&self, _: &RelationIdentity) -> Option<SequenceSecurity> {
        assert!(self.held.get());
        self.events.borrow_mut().push("security");
        Some(SequenceSecurity {
            role_owner: "uqa".into(),
            acl: None,
        })
    }
    fn sequence_persistence(&self, _: &RelationIdentity) -> Option<RelationPersistence> {
        assert!(self.held.get());
        self.events.borrow_mut().push("persistence");
        None
    }
}

#[test]
fn sequence_catalog_guard_covers_metadata_reads_and_releases_on_error() {
    let relation = RelationIdentity::from_legacy_name("public.counter").unwrap();
    let object_id = [7; 16];
    let state = SequenceState::initial(1, 1, SequenceDataType::BigInt);
    let mut catalog = TestCatalog {
        ids: BTreeMap::from([(relation.clone(), object_id)]),
        states: BTreeMap::from([(relation.clone(), state)]),
        held: Cell::new(false),
        events: RefCell::new(Vec::new()),
    };
    let sequence = read_sequence(&catalog, sequence_relation_oid(object_id))
        .unwrap()
        .unwrap();
    assert_eq!(sequence.relation, relation);
    assert_eq!(sequence.state, state);
    assert!(!catalog.held.get());
    assert_eq!(
        *catalog.events.borrow(),
        [
            "refresh",
            "object_ids",
            "state",
            "security",
            "persistence",
            "release"
        ]
    );

    catalog.states.clear();
    catalog.events.borrow_mut().clear();
    let error = read_sequence(&catalog, sequence_relation_oid(object_id))
        .err()
        .unwrap();
    assert!(error.to_string().contains("disappeared"), "{error}");
    assert!(!catalog.held.get());
    assert_eq!(
        *catalog.events.borrow(),
        ["refresh", "object_ids", "state", "release"]
    );
}
