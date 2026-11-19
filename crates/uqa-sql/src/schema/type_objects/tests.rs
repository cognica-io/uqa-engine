//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{
    domain::DomainCatalog,
    roles::{RoleReference, RoleReferenceNames},
    security::BoundSchemaSecurity,
};
use crate::schema::domains::removal::{TypeObjectAuthority, TypeObjectCatalog};
use std::{cell::RefCell, collections::BTreeMap};

#[derive(Default)]
struct Catalog {
    types: BTreeMap<Vec<String>, i64>,
    looked_up: RefCell<Vec<Vec<String>>>,
    temporary: Option<&'static str>,
    row_types: BTreeMap<u32, RowTypeRelation>,
    arrays: BTreeMap<u32, u32>,
}

impl DomainCatalog for Catalog {
    fn domain_by_oid(&self, _: u32) -> Option<StoredDomain> {
        None
    }
}

impl TypeObjectCatalog for Catalog {
    fn temporary_schema(&self) -> Option<String> {
        self.temporary.map(str::to_owned)
    }

    fn schema_security(&self, name: &str) -> Option<BoundSchemaSecurity> {
        (!matches!(name, "absent" | "pg_temp"))
            .then(|| BoundSchemaSecurity::owner(RoleIdentity::BOOTSTRAP))
    }

    fn resolve_drop_type_oid(&self, name: &str) -> Result<Option<i64>, SQLError> {
        let parsed = crate::parse_regtype_name(name)?.expect("type lookup name");
        self.looked_up.borrow_mut().push(parsed.names.clone());
        Ok(self.types.get(&parsed.names).copied())
    }

    fn format_drop_type(&self, oid: i64) -> Result<Option<String>, String> {
        Ok((oid == 23).then(|| "integer".into()))
    }

    fn enum_by_type_oid(&self, _: u32) -> Option<StoredEnum> {
        None
    }
    fn composite_by_type_oid(&self, _: u32) -> Option<StoredComposite> {
        None
    }
    fn user_array_element(&self, oid: u32) -> Option<u32> {
        self.arrays.get(&oid).copied()
    }
    fn row_type_relation(&self, oid: u32) -> Option<RowTypeRelation> {
        self.row_types.get(&oid).cloned()
    }
}

#[test]
fn resolved_type_description_lifetime_follows_relations_and_array_elements() {
    use crate::ast::RelationPersistence;
    let mut catalog = Catalog::default();
    for (oid, schema, persistence) in [
        (20_001, "public", RelationPersistence::Temporary),
        (20_002, "pg_temp_42", RelationPersistence::Permanent),
        (20_003, "public", RelationPersistence::Unlogged),
    ] {
        let name = format!("row_{oid}");
        let owner = RoleIdentity {
            oid: i64::from(oid) + 50_000,
            object_id: [7; 16],
        };
        catalog.types.insert(vec![name.clone()], i64::from(oid));
        catalog.row_types.insert(
            oid,
            RowTypeRelation {
                kind: "table",
                name: name.clone(),
                local_name: name.clone(),
                owner,
                schema: schema.into(),
                persistence,
            },
        );
        catalog
            .types
            .insert(vec![format!("array_{oid}")], i64::from(oid + 10));
        catalog.arrays.insert(oid + 10, oid);
        for name in [name, format!("array_{oid}")] {
            assert_eq!(
                resolve_type_object(&context(&catalog), &name)
                    .unwrap()
                    .owner(&context(&catalog)),
                owner
            );
            assert_eq!(
                resolve_type_object(&context(&catalog), &name)
                    .unwrap()
                    .persistence(&context(&catalog))
                    .unwrap(),
                persistence
            );
        }
    }
    catalog
        .types
        .insert(vec!["pg_catalog".into(), "int4".into()], 23);
    assert_eq!(
        resolve_type_object(&context(&catalog), "integer")
            .unwrap()
            .persistence(&context(&catalog))
            .unwrap(),
        RelationPersistence::Permanent
    );
}

impl TypeObjectAuthority for Catalog {
    fn schema_usage(&self, schema: &str, _: &RoleReference) -> bool {
        schema != "private"
    }

    fn current_user_has_role_privileges(
        &self,
        _: &dyn crate::catalog::roles::identity::RoleSubject,
    ) -> bool {
        true
    }
}

impl RoleReferenceNames for Catalog {
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

fn context(catalog: &Catalog) -> TypeObjectBinding<'_> {
    TypeObjectBinding {
        catalog,
        authority: catalog,
        session: catalog,
    }
}

#[test]
fn temporary_type_namespaces_resolve_before_schema_authority_checks() {
    let mut catalog = Catalog {
        types: BTreeMap::from([(vec!["pg_temp".into(), "object".into()], 23)]),
        ..Catalog::default()
    };
    assert_eq!(
        resolve_type_object(&context(&catalog), "pg_temp.object")
            .unwrap_err()
            .sqlstate(),
        Some("3F000")
    );
    assert!(catalog.looked_up.borrow().is_empty());
    catalog.temporary = Some("pg_temp_42");
    assert_eq!(
        resolve_type_object(&context(&catalog), "pg_temp.object")
            .unwrap()
            .oid(),
        23
    );
    catalog.temporary = Some("private");
    assert_eq!(
        resolve_type_object(&context(&catalog), "pg_temp.object")
            .unwrap_err()
            .sqlstate(),
        Some("42501")
    );
    assert_eq!(catalog.looked_up.borrow().len(), 1);
}

#[test]
fn named_type_targets_preserve_identifiers_that_are_sql_type_aliases() {
    let mut catalog = Catalog {
        types: BTreeMap::from([(vec!["pg_catalog".into(), "int4".into()], 23)]),
        ..Catalog::default()
    };
    let error = resolve_named_type_object(&context(&catalog), "integer").unwrap_err();
    assert_eq!(error.sqlstate(), Some("42704"));
    assert_eq!(error.to_string(), "type \"integer\" does not exist");
    assert_eq!(
        resolve_type_object(&context(&catalog), "integer")
            .unwrap()
            .oid(),
        23
    );
    let system = resolve_named_type_object(&context(&catalog), "pg_catalog.int4").unwrap();
    assert_eq!(system.oid(), 23);
    let error = system
        .require_domain_keyword(&context(&catalog), TypeObjectKind::Domain)
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42809"));
    assert_eq!(error.to_string(), "integer is not a domain");

    catalog.types.insert(vec!["integer".into()], 20_000);
    assert_eq!(
        resolve_named_type_object(&context(&catalog), "integer")
            .unwrap()
            .oid(),
        20_000
    );
    catalog
        .types
        .insert(vec!["Mixed.Schema".into(), "quoted\"name".into()], 20_001);
    assert_eq!(
        resolve_named_type_object(&context(&catalog), "\"Mixed.Schema\".\"quoted\"\"name\"")
            .unwrap()
            .oid(),
        20_001
    );
}

#[test]
fn named_type_targets_keep_namespace_validation_before_type_lookup() {
    let catalog = Catalog::default();
    for (name, state, message) in [
        (
            "absent.integer",
            "3F000",
            "schema \"absent\" does not exist",
        ),
        (
            "private.integer",
            "42501",
            "permission denied for schema private",
        ),
    ] {
        let error = resolve_named_type_object(&context(&catalog), name).unwrap_err();
        assert_eq!(error.sqlstate(), Some(state));
        assert_eq!(error.to_string(), message);
    }
    assert!(catalog.looked_up.borrow().is_empty());
}
