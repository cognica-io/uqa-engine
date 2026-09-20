//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

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
            identity: RelationIdentity::new("public", "positive"),
            owner: RoleIdentity::BOOTSTRAP,
            definition,
        },
    )])
}

#[test]
fn domain_constraint_oids_and_incarnations_are_unique_across_the_registry() {
    let original = registry();
    let roles = BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())]);
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
    let mut roles = BTreeMap::from([("renamed".into(), owner), ("uqa".into(), replacement)]);
    validate_domain_registry(&registry, &roles).unwrap();
    roles.remove("renamed");
    assert!(validate_domain_registry(&registry, &roles)
        .unwrap_err()
        .contains("missing role incarnation"));
    roles.get_mut("uqa").unwrap().oid = RoleIdentity::BOOTSTRAP.oid;
    assert!(validate_domain_registry(&registry, &roles).is_err());
}

#[test]
fn domain_candidates_reject_inconsistent_names_and_duplicate_or_invalid_identities() {
    let original = registry();
    let roles = BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())]);
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
