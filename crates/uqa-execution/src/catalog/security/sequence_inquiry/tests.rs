//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{security::roles::persistence::RoleCatalogSnapshot, sequence::SequenceState};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    sync::Arc,
};
use uqa_core::{catalog_sequence::SequencePrivileges, RelationIdentity};
use uqa_sql::{
    ast::{RelationPersistence, SequenceDataType},
    catalog::{
        resolution::RelationResolution,
        roles::{identity::RoleBinding, RoleDefinition, RoleReference, RoleReferenceNames},
        security::{sequence_inquiry::SequencePrivilegeResolution, SequenceSecurity},
    },
};
use uqa_storage::StorageBackendResult;

struct Catalog {
    retained: SequenceReadSnapshot,
    current: RefCell<SequenceReadSnapshot>,
    after_resolution: RefCell<Option<SequenceReadSnapshot>>,
    reads: Cell<usize>,
    resolutions: Cell<usize>,
}

impl Catalog {
    fn new() -> Self {
        let mut reader = RoleDefinition::bootstrap();
        reader.name = "reader".into();
        reader.oid = 20_000;
        reader.object_id = [1; 16];
        reader.attributes.clear();
        let relation = RelationIdentity::new("public", "ids");
        let retained = SequenceReadSnapshot {
            sequences: Arc::new(BTreeMap::from([(
                relation.clone(),
                SequenceState::initial(1, 1, SequenceDataType::BigInt),
            )])),
            object_ids: Arc::new(BTreeMap::from([(relation.clone(), [7; 16])])),
            persistence: Arc::new(BTreeMap::from([(
                relation.clone(),
                RelationPersistence::Permanent,
            )])),
            security: Arc::new(BTreeMap::from([(
                relation,
                SequenceSecurity {
                    role_owner: "uqa".into(),
                    acl: Some(vec![uqa_core::catalog_sequence::SequenceAclEntry {
                        role: "reader".into(),
                        grantor: Some("uqa".into()),
                        privileges: SequencePrivileges::ALL,
                        grant_options: SequencePrivileges::default(),
                    }]),
                },
            )])),
            roles: RoleCatalogSnapshot {
                roles: Arc::new(BTreeMap::from([
                    ("uqa".into(), RoleDefinition::bootstrap()),
                    (reader.name.clone(), reader),
                ])),
                memberships: Arc::new(BTreeMap::new()),
            },
        };
        Self {
            current: RefCell::new(retained.clone()),
            retained,
            after_resolution: RefCell::new(None),
            reads: Cell::new(0),
            resolutions: Cell::new(0),
        }
    }

    fn value(&self, arguments: &[Value]) -> Result<Value, SQLError> {
        sequence_privilege_value(
            &SequencePrivilegeInquiry {
                names: self,
                roles: &self.retained,
                security: &self.retained,
                resolution: self,
            },
            self,
            arguments,
            |_| panic!("unexpected non-sequence lookup"),
        )
    }
}

impl SequenceSnapshotSource for Catalog {
    fn sequence_read_snapshot(&self) -> StorageBackendResult<SequenceReadSnapshot> {
        self.reads.set(self.reads.get() + 1);
        Ok(self.current.borrow().clone())
    }
}

impl RoleReferenceNames for Catalog {
    fn current_role(&self) -> RoleReference {
        RoleReference::Bound(Arc::new(
            RoleBinding::from_definition(&self.retained.roles.roles["reader"]).unwrap(),
        ))
    }
    fn session_role(&self) -> RoleReference {
        self.current_role()
    }
}

impl SequencePrivilegeResolution for Catalog {
    fn visible_relation_kind(&self, _: &str) -> Result<RelationResolution, SQLError> {
        self.resolutions.set(self.resolutions.get() + 1);
        if let Some(next) = self.after_resolution.borrow_mut().take() {
            *self.current.borrow_mut() = next;
        }
        Ok(RelationResolution::Found("public.ids".into(), "sequence"))
    }
    fn sequence_privilege_oid(
        &self,
        _: i64,
    ) -> Result<Option<(String, RelationIdentity)>, SQLError> {
        panic!("inquiry must resolve sequence OIDs from its detached snapshot")
    }
}

#[test]
fn inquiry_refresh_keeps_selected_role_identity_across_name_resolution() {
    for subject in [
        None,
        Some(Value::Str("reader".into())),
        Some(Value::Int(20_000)),
    ] {
        let catalog = Catalog::new();
        let mut replaced = catalog.retained.clone();
        Arc::make_mut(&mut replaced.roles.roles)
            .get_mut("reader")
            .unwrap()
            .object_id = [2; 16];
        *catalog.after_resolution.borrow_mut() = Some(replaced);
        let mut arguments = subject.into_iter().collect::<Vec<_>>();
        arguments.extend([Value::Str("ids".into()), Value::Str("USAGE".into())]);
        assert_eq!(catalog.value(&arguments).unwrap(), Value::Bool(false));
        assert_eq!(catalog.retained.roles.roles["reader"].object_id, [1; 16]);
        assert_eq!(catalog.resolutions.get(), 1);
    }
}

#[test]
fn inquiry_oid_authority_is_current_without_touching_live_resolution() {
    let catalog = Catalog::new();
    Arc::make_mut(&mut catalog.current.borrow_mut().security)
        .values_mut()
        .next()
        .unwrap()
        .acl = Some(Vec::new());
    let oid = sequence_relation_oid([7; 16]);
    for subject in [
        None,
        Some(Value::Str("reader".into())),
        Some(Value::Int(20_000)),
    ] {
        let mut arguments = subject.into_iter().collect::<Vec<_>>();
        arguments.extend([Value::Int(oid), Value::Str("USAGE".into())]);
        assert_eq!(catalog.value(&arguments).unwrap(), Value::Bool(false));
    }
    assert_eq!(catalog.resolutions.get(), 0);
    assert!(!catalog
        .retained
        .security
        .values()
        .next()
        .unwrap()
        .acl
        .as_ref()
        .unwrap()
        .is_empty());
}

#[test]
fn null_and_invalid_implicit_inquiries_do_not_load_catalogs() {
    let catalog = Catalog::new();
    assert_eq!(
        catalog
            .value(&[Value::Null, Value::Str("bad".into())])
            .unwrap(),
        Value::Null
    );
    assert_eq!(
        catalog
            .value(&[Value::Int(0), Value::Str("bad".into())])
            .unwrap_err()
            .sqlstate(),
        Some("22023")
    );
    assert!(matches!(catalog.value(&[]), Err(SQLError::BadArity { .. })));
    assert_eq!(catalog.reads.get(), 0);
    assert_eq!(catalog.resolutions.get(), 0);
}

#[test]
fn inquiry_binds_a_new_committed_role_before_target_resolution() {
    let catalog = Catalog::new();
    {
        let mut snapshot = catalog.current.borrow_mut();
        let mut role = snapshot.roles.roles["reader"].clone();
        role.name = "new_reader".into();
        role.oid = 20_001;
        role.object_id = [2; 16];
        Arc::make_mut(&mut snapshot.roles.roles).insert(role.name.clone(), role);
        Arc::make_mut(&mut snapshot.security)
            .values_mut()
            .next()
            .unwrap()
            .acl
            .as_mut()
            .unwrap()[0]
            .role = "new_reader".into();
    }
    for subject in [Value::Str("new_reader".into()), Value::Int(20_001)] {
        for target in [
            Value::Str("ids".into()),
            Value::Int(sequence_relation_oid([7; 16])),
        ] {
            assert_eq!(
                catalog
                    .value(&[subject.clone(), target, Value::Str("USAGE".into())])
                    .unwrap(),
                Value::Bool(true)
            );
        }
    }
    assert!(!catalog.retained.roles.roles.contains_key("new_reader"));
}

#[test]
fn removed_sequence_authority_does_not_reappear_from_a_retained_statement_catalog() {
    let catalog = Catalog::new();
    Arc::make_mut(&mut catalog.current.borrow_mut().object_ids).clear();
    let oid = sequence_relation_oid([7; 16]);
    let arguments = [Value::Int(oid), Value::Str("USAGE".into())];
    let inquiry = SequencePrivilegeInquiry {
        names: &catalog,
        roles: &catalog.retained,
        security: &catalog.retained,
        resolution: &catalog,
    };
    for kind in [None, Some("S"), Some("r")] {
        let result = sequence_privilege_value(&inquiry, &catalog, &arguments, |requested| {
            assert_eq!(requested, oid);
            Ok(kind.map(|kind| ("ids".into(), kind.into())))
        });
        if kind == Some("r") {
            assert_eq!(result.unwrap_err().sqlstate(), Some("42809"));
        } else {
            assert_eq!(result.unwrap(), Value::Null);
        }
    }
    assert_eq!(
        catalog
            .value(&[Value::Str("ids".into()), Value::Str("USAGE".into())])
            .unwrap_err()
            .sqlstate(),
        Some("42P01")
    );
}
