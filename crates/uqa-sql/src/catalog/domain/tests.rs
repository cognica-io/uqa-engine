//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::roles::RoleDefinition;
use crate::catalog::roles::RoleDefinitions;
use std::collections::{BTreeMap, BTreeSet};

#[test]
fn domain_defaults_select_the_owned_declaration_without_reviving_removed_base_defaults() {
    struct Catalog(Vec<StoredDomain>);
    impl DomainCatalog for Catalog {
        fn domain_by_oid(&self, oid: u32) -> Option<StoredDomain> {
            self.0.iter().find(|domain| domain.oid == oid).cloned()
        }
    }
    let mut base = registry().remove("public.positive").unwrap();
    base.definition.default = Some(crate::ast::Expr::Literal(uqa_core::Value::Int(5)));
    let base_type = base.column_type();
    let mut derived = base.clone();
    derived.oid += 1;
    derived.identity.name = "derived".into();
    derived.definition.base = base_type;
    let derived_type = derived.column_type();
    let mut catalog = Catalog(vec![base, derived]);
    catalog.0[0].definition.default = Some(crate::ast::Expr::Literal(uqa_core::Value::Int(9)));
    assert!(matches!(
        domain_default_expression(&catalog, &derived_type),
        Some(crate::ast::Expr::Literal(uqa_core::Value::Int(5)))
    ));
    catalog.0[1].definition.default = None;
    assert!(domain_default_expression(&catalog, &derived_type).is_none());
    catalog.0[1].definition.default = Some(crate::ast::Expr::Literal(uqa_core::Value::Null));
    assert!(matches!(
        domain_default_expression(&catalog, &derived_type),
        Some(crate::ast::Expr::Literal(uqa_core::Value::Null))
    ));
}

fn registry() -> BTreeMap<String, StoredDomain> {
    let crate::Statement::CreateDomain(mut definition) =
        crate::compile("CREATE DOMAIN public.positive AS integer CHECK (VALUE > 0)")
            .unwrap()
            .remove(0)
    else {
        unreachable!()
    };
    crate::schema::domains::constraints::assign_names(&mut definition, &BTreeSet::new()).unwrap();
    crate::schema::domains::constraints::materialize(&mut definition, &mut |_: &str| Ok([7; 16]))
        .unwrap();
    BTreeMap::from([(
        "public.positive".into(),
        StoredDomain {
            object_id: [1; 16],
            oid: domain_object_oid(&[1; 16]),
            array_oid: None,
            identity: RelationIdentity::new("public", "positive"),
            owner: RoleIdentity::BOOTSTRAP,
            definition,
            array_name: None,
            usage_acl: None,
        },
    )])
}

#[test]
fn domain_constraint_oids_and_incarnations_are_unique_across_the_registry() {
    let original = registry();
    let roles =
        RoleDefinitions::try_from_iter([("uqa".into(), RoleDefinition::bootstrap())]).unwrap();
    for duplicate_oid in [false, true] {
        let mut registry = original.clone();
        let mut domain = registry["public.positive"].clone();
        domain.object_id = [2; 16];
        domain.oid = domain_object_oid(&domain.object_id);
        domain.identity.name = "other".into();
        domain.definition.name = "public.other".into();
        let identity = domain.definition.checks[0]
            .catalog_identity
            .as_mut()
            .unwrap();
        if duplicate_oid {
            identity.object_id = [8; 16];
        } else {
            identity.oid += 1;
        }
        registry.insert("public.other".into(), domain);
        assert!(validate_domain_registry(&registry, &roles)
            .unwrap_err()
            .contains("duplicate domain constraint"));
    }
}

#[test]
fn domain_authority_follows_the_original_incarnation_through_rename_and_name_reuse() {
    let registry = registry();
    let mut owner = RoleDefinition::bootstrap();
    owner.name = "renamed".into();
    let mut replacement = RoleDefinition::bootstrap();
    replacement.object_id = [9; 16];
    replacement.oid = 42;
    let mut roles =
        RoleDefinitions::try_from_iter([("renamed".into(), owner), ("uqa".into(), replacement)])
            .unwrap();
    validate_domain_registry(&registry, &roles).unwrap();
    roles.remove("renamed").unwrap();
    assert!(validate_domain_registry(&registry, &roles)
        .unwrap_err()
        .contains("missing role incarnation"));
    roles
        .edit("uqa", |value| {
            value.oid = RoleIdentity::BOOTSTRAP.oid;
            Ok(())
        })
        .unwrap()
        .unwrap();
    assert!(validate_domain_registry(&registry, &roles).is_err());
}

#[test]
fn domain_candidates_reject_inconsistent_names_and_duplicate_or_invalid_identities() {
    let original = registry();
    let roles =
        RoleDefinitions::try_from_iter([("uqa".into(), RoleDefinition::bootstrap())]).unwrap();
    validate_domain_registry(&original, &roles).unwrap();
    for corruption in 0..6 {
        let mut registry = original.clone();
        let domain = registry.get_mut("public.positive").unwrap();
        match corruption {
            0 => domain.object_id = [0; 16],
            1 => domain.oid = 0,
            2 => domain.identity.name = "renamed".into(),
            3 => domain.definition.name = "public.wrong".into(),
            4 => domain.owner.object_id = [0; 16],
            _ => {
                let mut duplicate = domain.clone();
                duplicate.identity.name = "duplicate".into();
                duplicate.definition.name = "public.duplicate".into();
                registry.insert("public.duplicate".into(), duplicate);
            }
        }
        assert!(
            validate_domain_registry(&registry, &roles).is_err(),
            "corruption {corruption}"
        );
    }
}
