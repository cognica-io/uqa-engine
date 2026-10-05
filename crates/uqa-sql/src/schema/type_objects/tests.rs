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
}

impl DomainCatalog for Catalog {
    fn domain_by_oid(&self, _: u32) -> Option<StoredDomain> {
        None
    }
}

impl TypeObjectCatalog for Catalog {
    fn schema_security(&self, name: &str) -> Option<BoundSchemaSecurity> {
        (name != "absent").then(|| BoundSchemaSecurity::owner(RoleIdentity::BOOTSTRAP))
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
    fn user_array_element(&self, _: u32) -> Option<u32> {
        None
    }
    fn row_type_relation(&self, _: u32) -> Option<RowTypeRelation> {
        None
    }
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
