//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn definition() -> CreateDomain {
    let crate::Statement::CreateDomain(definition) = crate::compile(
        "CREATE DOMAIN public.positive AS int NOT NULL CHECK(VALUE>0) CONSTRAINT upper CHECK(VALUE<100)",
    ).unwrap().remove(0) else { unreachable!() };
    definition
}

#[test]
fn each_domain_constraint_retains_its_own_identity_after_serialization() {
    let mut definition = definition();
    assign_names(&mut definition, &BTreeSet::from(["positive_check".into()])).unwrap();
    assert_eq!(
        definition.checks[0].name.as_deref(),
        Some("positive_check1")
    );
    let mut next = 0_u8;
    materialize(&mut definition, &mut |_: &str| {
        next += 1;
        Ok([next; 16])
    })
    .unwrap();
    assert_eq!(next, 3);
    let expected = identities(&definition).collect::<Vec<_>>();
    let mut restored: CreateDomain =
        serde_json::from_str(&serde_json::to_string(&definition).unwrap()).unwrap();
    let changed = materialize(&mut restored, &mut |_: &str| -> ConstraintMetadataResult<
        [u8; 16],
    > {
        panic!("existing domain constraint identity was replaced")
    })
    .unwrap();
    assert!(!changed);
    assert_eq!(identities(&restored).collect::<Vec<_>>(), expected);
    validate(&restored, false).unwrap();
}

#[test]
fn legacy_constraints_need_conversion_and_supplied_corruption_never_gets_repaired() {
    let mut value = serde_json::to_value(definition()).unwrap();
    value["not_null"]
        .as_object_mut()
        .unwrap()
        .remove("catalog_identity");
    for check in value["checks"].as_array_mut().unwrap() {
        check.as_object_mut().unwrap().remove("catalog_identity");
    }
    let original: CreateDomain = serde_json::from_value(value).unwrap();
    validate(&original, true).unwrap();
    assert!(validate(&original, false).is_err());
    for corruption in 0..3 {
        let mut candidate = original.clone();
        assign_names(&mut candidate, &BTreeSet::new()).unwrap();
        let identity = ConstraintCatalogIdentity {
            object_id: [7; 16],
            oid: 50_001,
        };
        candidate.not_null.as_mut().unwrap().catalog_identity = Some(identity);
        candidate.checks[0].catalog_identity = Some(match corruption {
            0 => ConstraintCatalogIdentity {
                object_id: [0; 16],
                ..identity
            },
            1 => ConstraintCatalogIdentity {
                object_id: [8; 16],
                ..identity
            },
            _ => ConstraintCatalogIdentity {
                oid: 50_002,
                ..identity
            },
        });
        assert!(
            materialize(&mut candidate, &mut |_: &str| -> ConstraintMetadataResult<
                [u8; 16],
            > {
                panic!("corrupt supplied metadata reached allocation")
            })
            .is_err()
        );
    }
}

#[test]
fn existing_addresses_are_reserved_before_any_missing_identity_is_allocated() {
    use crate::schema::constraint_metadata::CatalogObjectAllocator;
    struct Allocator {
        included: Vec<ConstraintCatalogIdentity>,
    }
    impl CatalogObjectAllocator for Allocator {
        fn include_catalog_identity(
            &mut self,
            relation: &RelationIdentity,
            class: CatalogOidClass,
            identity: ConstraintCatalogIdentity,
        ) -> ConstraintMetadataResult<()> {
            assert_eq!(relation.qualified_name(), "public.positive");
            assert_eq!(class, CatalogOidClass::Constraint);
            self.included.push(identity);
            Ok(())
        }
        fn allocate_object_id(&mut self, _: &str) -> ConstraintMetadataResult<[u8; 16]> {
            assert_eq!(self.included.len(), 2);
            Ok([9; 16])
        }
        fn allocate_catalog_oid(
            &mut self,
            class: CatalogOidClass,
            _: &[u8; 16],
        ) -> ConstraintMetadataResult<i64> {
            assert_eq!(class, CatalogOidClass::Constraint);
            Ok(50_003)
        }
    }
    let mut definition = definition();
    assign_names(&mut definition, &BTreeSet::new()).unwrap();
    definition.not_null.as_mut().unwrap().catalog_identity = Some(ConstraintCatalogIdentity {
        object_id: [7; 16],
        oid: 50_001,
    });
    definition.checks[1].catalog_identity = Some(ConstraintCatalogIdentity {
        object_id: [8; 16],
        oid: 50_002,
    });
    let mut allocator = Allocator {
        included: Vec::new(),
    };
    assert!(materialize(&mut definition, &mut allocator).unwrap());
    assert_eq!(definition.checks[0].catalog_identity.unwrap().oid, 50_003);
}
