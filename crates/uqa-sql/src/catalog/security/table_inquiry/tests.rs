//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{
    roles::{
        guards::{RoleDefinitionRead, RoleMembershipRead},
        RoleMembership, RoleMembershipKey,
    },
    security::{
        columns::grant_column_acl,
        sequence::AclPrivilege,
        sequence_inquiry::{
            SequencePrivilegeInquiry, SequencePrivilegeResolution, SequenceSecurityCatalog,
            SequenceSecurityRead,
        },
        table::TableAclPrivilege,
        AclGrantee, BoundSequenceSecurity, TableAclEntry, TablePrivileges,
    },
};
use std::cell::{Cell, RefCell};
use uqa_core::catalog_sequence::SequencePrivileges;

struct Catalog {
    roles: RefCell<BTreeMap<String, RoleDefinition>>,
    memberships: BTreeMap<RoleMembershipKey, RoleMembership>,
    table_security: TableSecurity,
    sequences: RefCell<BTreeMap<RelationIdentity, BoundSequenceSecurity>>,
    replace_subject_on_lookup: Cell<bool>,
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
            roles: RefCell::new(BTreeMap::from([
                (owner.name.clone(), owner),
                (reader.name.clone(), reader.clone()),
            ])),
            memberships: BTreeMap::new(),
            table_security: TableSecurity {
                acl: Some(vec![TableAclEntry {
                    role: "reader".into(),
                    grantor: Some("uqa".into()),
                    privileges: TableAclPrivilege::Select.mask(),
                    grant_options: TablePrivileges::default(),
                }]),
                ..TableSecurity::owner("uqa")
            },
            sequences: RefCell::new(BTreeMap::from([(
                RelationIdentity::new("public", "ids"),
                BoundSequenceSecurity {
                    role_owner: uqa_core::catalog_role::RoleIdentity::BOOTSTRAP,
                    acl: Some(vec![uqa_core::catalog_role::BoundAclEntry {
                        role: Some(reader.identity()),
                        grantor: uqa_core::catalog_role::RoleIdentity::BOOTSTRAP,
                        privileges: AclPrivilege::Select.mask(),
                        grant_options: SequencePrivileges::default(),
                    }]),
                },
            )])),
            replace_subject_on_lookup: Cell::new(false),
        }
    }

    fn lookup(&self) {
        if self.replace_subject_on_lookup.replace(false) {
            self.roles.borrow_mut().get_mut("reader").unwrap().object_id = [2; 16];
            self.sequences
                .borrow_mut()
                .values_mut()
                .next()
                .unwrap()
                .acl
                .as_mut()
                .unwrap()[0]
                .role = Some(self.roles.borrow()["reader"].identity());
        }
    }

    fn evaluate(&self, column: bool, arguments: &[Value]) -> Result<Value, SQLError> {
        let sequences = SequencePrivilegeInquiry {
            names: self,
            roles: self,
            security: self,
            resolution: self,
        };
        let inquiry = TablePrivilegeInquiry {
            names: self,
            roles: self,
            sequences: &sequences,
            catalog: self,
        };
        if column {
            inquiry.has_column_privilege_value(arguments)
        } else {
            inquiry.has_table_privilege_value(arguments)
        }
    }
}

impl RoleReferenceNames for Catalog {
    fn current_role(&self) -> RoleReference {
        "reader".into()
    }
    fn session_role(&self) -> RoleReference {
        "reader".into()
    }
}

impl RoleCatalogGuards for Catalog {
    fn role_definitions(&self) -> RoleDefinitionRead<'_> {
        Box::new(self.roles.borrow())
    }
    fn role_memberships(&self) -> RoleMembershipRead<'_> {
        Box::new(&self.memberships)
    }
}

impl SequenceSecurityCatalog for Catalog {
    fn security_read(&self) -> SequenceSecurityRead<'_> {
        Box::new(self.sequences.borrow())
    }
}

impl SequencePrivilegeResolution for Catalog {
    fn visible_relation_kind(&self, reference: &str) -> Result<RelationResolution, SQLError> {
        TablePrivilegeCatalog::visible_relation_kind(self, reference)
    }
    fn sequence_privilege_oid(
        &self,
        oid: i64,
    ) -> Result<Option<(String, RelationIdentity)>, SQLError> {
        self.lookup();
        Ok((oid == 30_001).then(|| ("public.ids".into(), RelationIdentity::new("public", "ids"))))
    }
}

impl TablePrivilegeCatalog for Catalog {
    fn visible_relation_kind(&self, reference: &str) -> Result<RelationResolution, SQLError> {
        self.lookup();
        Ok(match reference {
            "items" => RelationResolution::Found("public.items".into(), "table"),
            "ids" => RelationResolution::Found("public.ids".into(), "sequence"),
            _ => RelationResolution::MissingRelation,
        })
    }
    fn resolve_table_privilege_oid(
        &self,
        oid: i64,
    ) -> Result<Option<ResolvedTablePrivilegeTarget>, SQLError> {
        self.lookup();
        Ok(match oid {
            30_000 => Some(ResolvedTablePrivilegeTarget::Table(RelationIdentity::new(
                "public", "items",
            ))),
            30_001 => Some(ResolvedTablePrivilegeTarget::Sequence(
                RelationIdentity::new("public", "ids"),
            )),
            _ => None,
        })
    }
    fn table_privilege_security(
        &self,
        _target: &ResolvedTablePrivilegeTarget,
        _roles: &BTreeMap<String, RoleDefinition>,
    ) -> Result<TableSecurity, SQLError> {
        Ok(self.table_security.clone())
    }
    fn column_privilege_relation(
        &self,
        _target: &ResolvedTablePrivilegeTarget,
        _roles: &BTreeMap<String, RoleDefinition>,
    ) -> Result<ColumnPrivilegeRelation, SQLError> {
        Ok(ColumnPrivilegeRelation {
            relation: RelationIdentity::new("public", "items"),
            security: self.table_security.clone(),
            columns: vec!["a".into(), "b".into()],
            has_system_columns: true,
        })
    }
}

fn text(value: &str) -> Value {
    Value::Str(value.into())
}

#[test]
fn selected_table_and_column_subjects_do_not_follow_reused_role_names_or_oids() {
    for subject in [text("reader"), Value::Int(20_000)] {
        for target in [
            text("items"),
            Value::Int(30_000),
            text("ids"),
            Value::Int(30_001),
        ] {
            for column in [false, true] {
                let catalog = Catalog::new();
                catalog.replace_subject_on_lookup.set(true);
                let mut arguments = vec![subject.clone(), target.clone()];
                if column {
                    arguments.push(Value::Int(1));
                }
                arguments.push(text("SELECT"));
                assert_eq!(
                    catalog.evaluate(column, &arguments).unwrap(),
                    Value::Bool(false),
                    "{arguments:?}"
                );
            }
        }
    }
}

#[test]
fn public_subjects_receive_only_applicable_table_column_and_sequence_grants() {
    let mut catalog = Catalog::new();
    catalog.table_security.acl = Some(Vec::new());
    grant_column_acl(
        &mut catalog.table_security,
        "a",
        TableAclPrivilege::Select,
        &[AclGrantee::Public],
        "uqa",
        false,
    );
    catalog
        .sequences
        .get_mut()
        .values_mut()
        .next()
        .unwrap()
        .acl
        .as_mut()
        .unwrap()[0]
        .role = None;
    for subject in [text("public"), Value::Int(0), Value::Int(99_999)] {
        for (target, table_select) in [
            (text("items"), false),
            (Value::Int(30_000), false),
            (text("ids"), true),
            (Value::Int(30_001), true),
        ] {
            for (privilege, expected) in [
                ("SELECT", table_select),
                ("INSERT", false),
                ("SELECT WITH GRANT OPTION", false),
            ] {
                assert_eq!(
                    catalog
                        .evaluate(false, &[subject.clone(), target.clone(), text(privilege)])
                        .unwrap(),
                    Value::Bool(expected)
                );
            }
            for (privilege, expected) in [
                ("SELECT", true),
                ("INSERT", false),
                ("SELECT WITH GRANT OPTION", false),
            ] {
                assert_eq!(
                    catalog
                        .evaluate(
                            true,
                            &[
                                subject.clone(),
                                target.clone(),
                                Value::Int(1),
                                text(privilege)
                            ]
                        )
                        .unwrap(),
                    Value::Bool(expected)
                );
            }
        }
        assert_eq!(
            catalog
                .evaluate(true, &[subject, text("items"), text("b"), text("SELECT")])
                .unwrap(),
            Value::Bool(false)
        );
    }
}

#[test]
fn privilege_validation_precedes_missing_oid_and_invalid_attribute_null_results() {
    let catalog = Catalog::new();
    for subject in [None, Some(text("reader")), Some(Value::Int(0))] {
        for (column, arguments, sqlstate) in [
            (false, vec![text("missing"), text("bad")], "42P01"),
            (false, vec![Value::Int(0), text("bad")], "22023"),
            (
                true,
                vec![text("items"), text("missing"), text("bad")],
                "42703",
            ),
            (
                true,
                vec![text("ids"), text("missing"), text("bad")],
                "42703",
            ),
            (
                true,
                vec![text("items"), Value::Int(0), text("bad")],
                "22023",
            ),
            (true, vec![text("ids"), Value::Int(0), text("bad")], "22023"),
            (
                true,
                vec![Value::Int(0), text("missing"), text("bad")],
                "22023",
            ),
            (
                true,
                vec![Value::Int(0), Value::Int(0), text("bad")],
                "22023",
            ),
        ] {
            let arguments = subject.iter().cloned().chain(arguments).collect::<Vec<_>>();
            assert_eq!(
                catalog.evaluate(column, &arguments).unwrap_err().sqlstate(),
                Some(sqlstate),
                "{arguments:?}"
            );
        }
    }
}
