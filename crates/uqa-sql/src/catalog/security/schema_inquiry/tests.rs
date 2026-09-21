//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::roles::{
    guards::{RoleDefinitionRead, RoleMembershipRead},
    RoleMembership, RoleMembershipKey,
};
use std::cell::{Cell, RefCell};

struct Graphs;

impl GraphNamespaceRead for Graphs {
    fn names(&self) -> Box<dyn Iterator<Item = &str> + '_> {
        Box::new(["g"].into_iter())
    }
    fn contains(&self, name: &str) -> bool {
        name == "g"
    }
}

struct Fixture {
    schemas: BTreeMap<String, BoundSchemaSecurity>,
    roles: BTreeMap<String, RoleDefinition>,
    memberships: BTreeMap<RoleMembershipKey, RoleMembership>,
    reads: RefCell<Vec<Option<String>>>,
    failure: Cell<Option<&'static str>>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            schemas: [("public".into(), BoundSchemaSecurity::bootstrap("public"))].into(),
            roles: [("uqa".into(), RoleDefinition::bootstrap())].into(),
            memberships: BTreeMap::new(),
            reads: RefCell::new(Vec::new()),
            failure: Cell::new(None),
        }
    }

    fn inquiry(&self) -> SchemaPrivilegeInquiry<'_> {
        SchemaPrivilegeInquiry {
            catalog: self,
            names: self,
            roles: self,
        }
    }
}

impl SchemaPrivilegeCatalog for Fixture {
    fn refresh_namespace_catalog(&self) -> Result<(), SQLError> {
        Ok(())
    }
    fn schemas(&self) -> SchemaRegistryRead<'_> {
        Box::new(&self.schemas)
    }
    fn graphs(&self) -> Box<dyn GraphNamespaceRead + '_> {
        Box::new(Graphs)
    }
    fn temporary_namespace_allocated(&self) -> bool {
        false
    }
    fn temporary_schema_name(&self) -> String {
        "pg_temp_1".into()
    }
    fn observe_namespace_lookup(&self, name: Option<&str>) -> Result<(), SQLError> {
        self.reads.borrow_mut().push(name.map(str::to_owned));
        if let Some(state) = self.failure.get() {
            return Err(SQLError::Routine {
                sqlstate: state.into(),
                message: "namespace observation failed".into(),
            });
        }
        Ok(())
    }
}

impl RoleCatalogGuards for Fixture {
    fn role_definitions(&self) -> RoleDefinitionRead<'_> {
        Box::new(&self.roles)
    }
    fn role_memberships(&self) -> RoleMembershipRead<'_> {
        Box::new(&self.memberships)
    }
}

impl RoleReferenceNames for Fixture {
    fn current_role(&self) -> RoleReference {
        "uqa".into()
    }
    fn session_role(&self) -> RoleReference {
        self.current_role()
    }
    fn outer_role(&self) -> RoleReference {
        self.current_role()
    }
}

#[test]
fn privilege_targets_observe_selected_names_and_oid_absence_without_registry_reads() {
    let fixture = Fixture::new();
    let inquiry = fixture.inquiry();
    let oid = inquiry
        .schema_security_for_privilege("g")
        .unwrap()
        .namespace_oid("g");
    for (value, expected) in [
        (Value::Str("g".into()), Some("g")),
        (Value::FixedChar("public".into()), Some("public")),
        (Value::Int(oid), Some("g")),
        (Value::Int(i64::MAX), None),
    ] {
        fixture.reads.borrow_mut().clear();
        assert_eq!(
            inquiry
                .resolve_schema_privilege_target(&value)
                .unwrap()
                .as_deref(),
            expected
        );
        assert_eq!(&*fixture.reads.borrow(), &[expected.map(str::to_owned)]);
    }
    fixture.reads.borrow_mut().clear();
    let error = inquiry
        .resolve_schema_privilege_target(&Value::Str("missing".into()))
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("3F000"));
    assert_eq!(&*fixture.reads.borrow(), &[Some("missing".into())]);
}

#[test]
fn privilege_binding_null_and_invalid_subjects_do_not_observe_namespaces() {
    let fixture = Fixture::new();
    let inquiry = fixture.inquiry();
    assert!(inquiry.schema_has_privilege_for_role("g", "uqa", SchemaAclPrivilege::Usage));
    assert_eq!(
        inquiry
            .has_schema_privilege_value(&[Value::Null, Value::Str("USAGE".into())])
            .unwrap(),
        Value::Null
    );
    let error = inquiry
        .has_schema_privilege_value(&[
            Value::Str("missing_role".into()),
            Value::Str("g".into()),
            Value::Str("USAGE".into()),
        ])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42704"));
    assert!(fixture.reads.borrow().is_empty());
}

#[test]
fn privilege_target_observation_preserves_typed_errors() {
    let fixture = Fixture::new();
    for state in ["40001", "57014", "53200"] {
        fixture.failure.set(Some(state));
        let error = fixture
            .inquiry()
            .has_schema_privilege_value(&[Value::Str("g".into()), Value::Str("USAGE".into())])
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some(state));
        assert!(error.to_string().contains("namespace observation failed"));
    }
}
