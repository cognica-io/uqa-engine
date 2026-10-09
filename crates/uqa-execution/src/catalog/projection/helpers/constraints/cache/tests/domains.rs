//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn with_domains(catalog: &CatalogReadView, unrelated: usize) -> CatalogReadView {
    let mut snapshot = catalog.snapshot().clone();
    for position in 0..=unrelated {
        let Statement::CreateDomain(mut definition) = uqa_sql::compile(
            "CREATE DOMAIN positive AS integer CONSTRAINT required NOT NULL CONSTRAINT positive CHECK (VALUE > 0)",
        ).unwrap().remove(0) else {
            panic!("domain");
        };
        let oid = 90_000 + i64::try_from(position).unwrap() * 10;
        definition.not_null.as_mut().unwrap().catalog_identity = Some(CatalogObjectIdentity {
            object_id: (position as u128 + 1).to_le_bytes(),
            oid: oid + 2,
        });
        definition.checks[0].catalog_identity = Some(CatalogObjectIdentity {
            object_id: (position as u128 + 1000).to_le_bytes(),
            oid: oid + 3,
        });
        let domain = StoredDomain {
            object_id: (position as u128 + 2000).to_le_bytes(),
            oid: u32::try_from(oid).unwrap(),
            array_oid: Some(u32::try_from(oid + 1).unwrap()),
            identity: RelationIdentity::new("public", format!("positive_{position:03}")),
            owner: RoleIdentity::BOOTSTRAP,
            definition,
            array_name: None,
            usage_acl: None,
        };
        Arc::make_mut(&mut snapshot.definitions.domains)
            .insert(domain.identity.qualified_name(), domain);
    }
    CatalogReadView::new(snapshot)
}

#[test]
fn domain_inquiries_build_addresses_once_without_copying_definitions() {
    for unrelated in [0, 1, 128] {
        let catalog = with_domains(&fixture(unrelated), unrelated);
        let services = CatalogServices::default();
        let resolution = &services.resolution;
        let output = RegtypeOutputCache::default();
        let context = services.context(&catalog, &output);
        let before = DOMAIN_READS.get();
        assert_eq!(
            crate::catalog::projection::pg_get_constraintdef_value(&context, &[Value::Int(70_000)])
                .unwrap(),
            Value::Str("CHECK ((id > 0))".into())
        );
        assert_eq!(DOMAIN_READS.get(), before);
        let expected = &catalog.snapshot().definitions.domains["public.positive_000"]
            .definition
            .checks[0];
        for _ in 0..4 {
            let alias = catalog.clone();
            let Some(DomainConstraint::Check(check)) =
                domain_constraint_by_oid(&alias, resolution, 90_003).unwrap()
            else {
                panic!("check");
            };
            assert!(std::ptr::eq(check, expected));
            assert!(matches!(
                domain_constraint_by_oid(&alias, resolution, 90_002).unwrap(),
                Some(DomainConstraint::NotNull)
            ));
            assert!(domain_constraint_by_oid(&alias, resolution, -1)
                .unwrap()
                .is_none());
            for (oid, definition) in [(90_002, "NOT NULL"), (90_003, "CHECK ((VALUE > 0))")] {
                assert_eq!(
                    crate::catalog::projection::pg_get_constraintdef_value(
                        &context,
                        &[Value::Int(oid)]
                    )
                    .unwrap(),
                    Value::Str(definition.into())
                );
            }
        }
        assert_eq!(DOMAIN_READS.get() - before, unrelated + 1);
    }
}

#[test]
fn concurrent_domain_readers_collect_addresses_once() {
    let catalog = with_domains(&fixture(0), 128);
    let resolution = CatalogServices::default().resolution;
    let ready = std::sync::Barrier::new(8);
    let reads = std::thread::scope(|scope| {
        let workers = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    ready.wait();
                    let before = DOMAIN_READS.get();
                    assert!(matches!(
                        domain_constraint_by_oid(&catalog, &resolution, 90_003).unwrap(),
                        Some(DomainConstraint::Check(_))
                    ));
                    DOMAIN_READS.get() - before
                })
            })
            .collect::<Vec<_>>();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .sum::<usize>()
    });
    assert_eq!(reads, 129);
}

#[test]
fn domain_addresses_preserve_first_match_and_ignore_unassigned_identities() {
    let catalog = with_domains(&fixture(0), 1);
    let mut snapshot = catalog.snapshot().clone();
    let domains = Arc::make_mut(&mut snapshot.definitions.domains);
    let first = domains.get_mut("public.positive_000").unwrap();
    first.definition.checks[0]
        .catalog_identity
        .as_mut()
        .unwrap()
        .oid = 90_002;
    let second = domains.get_mut("public.positive_001").unwrap();
    second
        .definition
        .not_null
        .as_mut()
        .unwrap()
        .catalog_identity
        .as_mut()
        .unwrap()
        .oid = 90_002;
    second.definition.checks[0].catalog_identity = None;
    let catalog = CatalogReadView::new(snapshot);
    let resolution = CatalogServices::default().resolution;
    assert!(matches!(
        domain_constraint_by_oid(&catalog, &resolution, 90_002).unwrap(),
        Some(DomainConstraint::NotNull)
    ));
    assert!(domain_constraint_by_oid(&catalog, &resolution, 90_013)
        .unwrap()
        .is_none());
    // Full pg_constraint still diagnoses the malformed declaration; point inquiry has always skipped it.
    let error = crate::catalog::projection::pg_catalog::build_pg_constraint(&catalog, &resolution)
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("constraint of domain public.positive_001 has no catalog identity"));
}

#[test]
fn domain_addresses_remain_with_their_original_generation() {
    let catalog = with_domains(&fixture(0), 0);
    let resolution = CatalogServices::default().resolution;
    let Some(DomainConstraint::Check(original)) =
        domain_constraint_by_oid(&catalog, &resolution, 90_003).unwrap()
    else {
        panic!("check");
    };
    let mut snapshot = catalog.snapshot().clone();
    let domains = Arc::make_mut(&mut snapshot.definitions.domains);
    domains
        .get_mut("public.positive_000")
        .unwrap()
        .definition
        .checks[0]
        .validated = false;
    let current = CatalogReadView::new(snapshot.clone());
    let Some(DomainConstraint::Check(changed)) =
        domain_constraint_by_oid(&current, &resolution, 90_003).unwrap()
    else {
        panic!("check");
    };
    assert!(!changed.validated);
    assert!(original.validated);
    Arc::make_mut(&mut snapshot.definitions.domains).clear();
    let removed = CatalogReadView::new(snapshot);
    assert!(domain_constraint_by_oid(&removed, &resolution, 90_003)
        .unwrap()
        .is_none());
    let Some(DomainConstraint::Check(retained)) =
        domain_constraint_by_oid(&catalog, &resolution, 90_003).unwrap()
    else {
        panic!("check");
    };
    assert!(std::ptr::eq(retained, original));
}
