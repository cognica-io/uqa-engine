//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::RelationPersistence;
use crate::catalog::roles::{
    guards::{RoleDefinitionRead, RoleMembershipRead},
    RoleDefinition, RoleReference,
};
use crate::catalog::roles::{RoleDefinitions, RoleMemberships};
use crate::catalog::security::BoundSequenceSecurity;
use std::cell::Cell;
use uqa_core::catalog_sequence::SequencePrivileges;

struct Catalog {
    roles: RoleDefinitions,
    memberships: RoleMemberships,
    security: SequenceSecurities,
    lookups: Cell<usize>,
}

impl Catalog {
    fn new() -> Self {
        let owner = RoleDefinition::bootstrap();
        let mut reader = owner.clone();
        reader.name = "reader".into();
        reader.oid = 20_000;
        reader.object_id = [1; 16];
        reader.attributes.clear();
        Self {
            roles: RoleDefinitions::try_from_iter([
                (owner.name.clone(), owner),
                (reader.name.clone(), reader.clone()),
            ])
            .unwrap(),
            memberships: RoleMemberships::new(),
            security: SequenceSecurities::try_from_iter([(
                RelationIdentity::new("public", "ids"),
                RelationPersistence::Permanent,
                BoundSequenceSecurity {
                    role_owner: uqa_core::catalog_role::RoleIdentity::BOOTSTRAP,
                    acl: Some(vec![uqa_core::catalog_role::BoundAclEntry {
                        role: Some(reader.identity()),
                        grantor: uqa_core::catalog_role::RoleIdentity::BOOTSTRAP,
                        privileges: AclPrivilege::Usage.mask(),
                        grant_options: SequencePrivileges::default(),
                    }]),
                },
            )])
            .unwrap(),
            lookups: Cell::new(0),
        }
    }

    fn inquiry(&self) -> SequencePrivilegeInquiry<'_> {
        SequencePrivilegeInquiry {
            names: self,
            roles: self,
            security: self,
            resolution: self,
        }
    }
}

impl RoleReferenceNames for Catalog {
    fn outer_role(&self) -> crate::catalog::roles::RoleReference {
        self.current_role()
    }
    fn current_role(&self) -> RoleReference {
        "reader".into()
    }
    fn session_role(&self) -> RoleReference {
        "reader".into()
    }
}

impl RoleCatalogGuards for Catalog {
    fn role_definitions(&self) -> RoleDefinitionRead<'_> {
        Box::new(&self.roles)
    }
    fn role_memberships(&self) -> RoleMembershipRead<'_> {
        Box::new(&self.memberships)
    }
}

impl SequenceSecurityCatalog for Catalog {
    fn security_read(&self) -> SequenceSecurityRead<'_> {
        Box::new(&self.security)
    }
}

impl SequencePrivilegeResolution for Catalog {
    fn visible_relation_kind(&self, reference: &str) -> Result<RelationResolution, SQLError> {
        self.lookups.set(self.lookups.get() + 1);
        Ok(match reference {
            "ids" => RelationResolution::Found("public.ids".into(), "sequence"),
            "ordinary" => RelationResolution::Found("public.ordinary".into(), "table"),
            "missing.ids" => RelationResolution::MissingSchema("missing".into()),
            _ => RelationResolution::MissingRelation,
        })
    }
    fn sequence_privilege_oid(
        &self,
        oid: i64,
    ) -> Result<Option<(String, RelationIdentity)>, SQLError> {
        self.lookups.set(self.lookups.get() + 1);
        Ok((oid == 30_000).then(|| ("public.ids".into(), RelationIdentity::new("public", "ids"))))
    }
}

fn text(value: &str) -> Value {
    Value::Str(value.into())
}

#[test]
fn invalid_inquiry_arguments_do_not_resolve_a_target() {
    let catalog = Catalog::new();
    for arguments in [
        vec![text("missing"), text("bad")],
        vec![Value::Int(0), text("bad")],
        vec![text("reader"), text("ordinary"), text("bad")],
    ] {
        assert_eq!(
            catalog
                .inquiry()
                .has_sequence_privilege_value(&arguments)
                .unwrap_err()
                .sqlstate(),
            Some("22023")
        );
    }
    assert_eq!(
        catalog
            .inquiry()
            .has_sequence_privilege_value(&[text("absent"), text("missing"), text("bad")])
            .unwrap_err()
            .sqlstate(),
        Some("42704")
    );
    assert_eq!(
        catalog
            .inquiry()
            .has_sequence_privilege_value(&[Value::Null, text("bad")])
            .unwrap(),
        Value::Null
    );
    assert!(matches!(
        catalog.inquiry().has_sequence_privilege_value(&[]),
        Err(SQLError::BadArity { .. })
    ));
    assert_eq!(catalog.lookups.get(), 0);
}

#[test]
fn public_and_unknown_role_oids_use_only_public_grants() {
    let mut catalog = Catalog::new();
    catalog
        .security
        .edit(&RelationIdentity::new("public", "ids"), |security| {
            security.acl.as_mut().unwrap()[0].role = None;
            Ok(())
        })
        .unwrap();
    for subject in [text("public"), Value::Int(0), Value::Int(99_999)] {
        for (privilege, expected) in [
            ("USAGE", true),
            ("SELECT", false),
            ("USAGE WITH GRANT OPTION", false),
        ] {
            assert_eq!(
                catalog
                    .inquiry()
                    .has_sequence_privilege_value(&[subject.clone(), text("ids"), text(privilege)])
                    .unwrap(),
                Value::Bool(expected)
            );
        }
    }
    assert_eq!(
        catalog
            .inquiry()
            .has_sequence_privilege_value(&[text("PUBLIC"), text("ids"), text("USAGE")])
            .unwrap_err()
            .sqlstate(),
        Some("42704")
    );
}

#[test]
fn a_selected_inquiry_role_never_follows_a_reused_name_or_oid() {
    for subject in [text("reader"), Value::Int(20_000)] {
        let mut catalog = Catalog::new();
        let arguments = [subject, text("ids"), text("USAGE")];
        let request = SequencePrivilegeArguments::parse(&arguments)
            .unwrap()
            .unwrap()
            .bind(&catalog, &catalog)
            .unwrap();
        catalog
            .roles
            .edit("reader", |value| {
                value.object_id = [2; 16];
                Ok(())
            })
            .unwrap()
            .unwrap();
        let relation = RelationIdentity::new("public", "ids");
        let reader = catalog.roles["reader"].identity();
        catalog
            .security
            .edit(&relation, |security| {
                security.acl.as_mut().unwrap()[0].role = Some(reader);
                Ok(())
            })
            .unwrap();
        assert_eq!(
            request.evaluate(&relation, &catalog, &catalog).unwrap(),
            Value::Bool(false)
        );
        assert_eq!(
            catalog
                .inquiry()
                .has_sequence_privilege_value(&arguments)
                .unwrap(),
            Value::Bool(true)
        );
        catalog
            .security
            .edit(&relation, |security| {
                security.acl.as_mut().unwrap()[0].role = None;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            request.evaluate(&relation, &catalog, &catalog).unwrap(),
            Value::Bool(true)
        );
    }
}

#[test]
fn valid_inquiry_privileges_preserve_target_errors_and_absent_oid_null() {
    let catalog = Catalog::new();
    for (target, expected) in [
        ("missing", "42P01"),
        ("missing.ids", "3F000"),
        ("ordinary", "42809"),
    ] {
        assert_eq!(
            catalog
                .inquiry()
                .has_sequence_privilege_value(&[text(target), text("USAGE")])
                .unwrap_err()
                .sqlstate(),
            Some(expected)
        );
    }
    assert_eq!(
        catalog
            .inquiry()
            .has_sequence_privilege_value(&[Value::Int(0), text("USAGE")])
            .unwrap(),
        Value::Null
    );
    assert_eq!(
        catalog
            .inquiry()
            .has_sequence_privilege_value(&[Value::Int(30_000), text("USAGE")])
            .unwrap(),
        Value::Bool(true)
    );
}
