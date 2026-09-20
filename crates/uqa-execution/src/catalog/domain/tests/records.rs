//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::domain::records as persistence;

fn domain(name: &str, id: u8) -> StoredDomain {
    let mut domain = legacy()
        .remove("public.positive")
        .unwrap()
        .bind_owner(&fixture().1)
        .unwrap();
    domain.identity = RelationIdentity::new("public", name);
    domain.definition.name = domain.identity.qualified_name();
    domain.object_id = [id; 16];
    domain.oid = uqa_sql::catalog::domain::domain_object_oid(&domain.object_id);
    domain
}

fn registry(domains: Vec<StoredDomain>) -> DomainRegistry {
    domains
        .into_iter()
        .map(|domain| (domain.identity.qualified_name(), domain))
        .collect()
}

#[test]
fn authority_aggregate_conversion_is_initial_only_and_keeps_identities() {
    let (catalog, roles) = fixture();
    let original = registry(vec![domain("a", 1), domain("b", 2)]);
    let json = encode(&original, &roles).unwrap();
    catalog.set_metadata(DOMAINS_METADATA_KEY, &json).unwrap();
    assert!(restore(&catalog, &roles, false).is_err());
    let converted = restore(&catalog, &roles, true).unwrap();
    assert_eq!(
        serde_json::to_value(&converted).unwrap(),
        serde_json::to_value(&original).unwrap()
    );
    assert_eq!(
        catalog
            .get_metadata(DOMAINS_METADATA_KEY)
            .unwrap()
            .as_deref(),
        Some(r#"{"domain_catalog_format":2}"#)
    );
    assert_eq!(
        catalog
            .metadata_with_prefix(persistence::PREFIX)
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        serde_json::to_value(restore(&catalog, &roles, false).unwrap()).unwrap(),
        serde_json::to_value(original).unwrap()
    );
}

#[test]
fn domain_deltas_do_not_rewrite_or_delete_independent_records() {
    let (catalog, roles) = fixture();
    let initial = registry(vec![domain("a", 1)]);
    persistence::migrate(&catalog, &initial).unwrap();
    let peer = registry(vec![domain("a", 1), domain("b", 2)]);
    persistence::persist(&catalog, &initial, &peer).unwrap();
    let candidate = registry(vec![domain("a", 1), domain("c", 3)]);
    persistence::persist(&catalog, &initial, &candidate).unwrap();
    let restored = restore(&catalog, &roles, false).unwrap();
    assert_eq!(
        restored.keys().cloned().collect::<Vec<_>>(),
        ["public.a", "public.b", "public.c"]
    );
    let remaining = registry(vec![domain("c", 3)]);
    persistence::persist(&catalog, &candidate, &remaining).unwrap();
    let restored = restore(&catalog, &roles, false).unwrap();
    assert_eq!(
        restored.keys().cloned().collect::<Vec<_>>(),
        ["public.b", "public.c"]
    );
    assert_eq!(
        catalog
            .get_metadata(DOMAINS_METADATA_KEY)
            .unwrap()
            .as_deref(),
        Some(r#"{"domain_catalog_format":2}"#)
    );
}

#[test]
fn independent_domain_oid_claims_cannot_replace_an_existing_owner() {
    let (catalog, roles) = fixture();
    let initial = registry(vec![domain("a", 1)]);
    persistence::migrate(&catalog, &initial).unwrap();
    let candidate = registry(vec![domain("b", 1)]);
    let error = persistence::persist(&catalog, &DomainRegistry::new(), &candidate).unwrap_err();
    assert_eq!(
        uqa_sql::catalog::errors::storage_error("domain", &error).sqlstate(),
        Some("23505")
    );
    assert_eq!(
        restore(&catalog, &roles, false)
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        ["public.a"]
    );
}

#[test]
fn malformed_current_records_are_rejected_without_repair() {
    for corruption in 0..8 {
        let (catalog, roles) = fixture();
        let initial = registry(vec![domain("a", 1), domain("b", 2)]);
        persistence::migrate(&catalog, &initial).unwrap();
        let key = persistence::key("public.a");
        let mut value = serde_json::to_value(&initial["public.a"]).unwrap();
        match corruption {
            0 => {
                catalog.set_metadata(&key, "{").unwrap();
            }
            1 => {
                value["identity"]["name"] = "mismatched".into();
                catalog.set_metadata(&key, &value.to_string()).unwrap();
            }
            2 => {
                value["owner"]["object_id"] = serde_json::to_value([99_u8; 16]).unwrap();
                catalog.set_metadata(&key, &value.to_string()).unwrap();
            }
            3 => {
                catalog
                    .set_metadata(&persistence::key("public.b"), &value.to_string())
                    .unwrap();
            }
            4 => {
                catalog.delete_metadata(DOMAINS_METADATA_KEY).unwrap();
            }
            5 => {
                catalog
                    .set_metadata(
                        DOMAINS_METADATA_KEY,
                        r#"{"domain_catalog_format":2,"domains":{}}"#,
                    )
                    .unwrap();
            }
            6 => {
                catalog
                    .set_metadata("uqa.sql.domain_oid.v1:123", "public.a")
                    .unwrap();
            }
            7 => {
                catalog
                    .delete_metadata(&format!(
                        "uqa.sql.domain_oid.v1:{}",
                        initial["public.a"].oid
                    ))
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let before = catalog.metadata_with_prefix("").unwrap();
        for allow_migration in [false, true] {
            assert!(
                restore(&catalog, &roles, allow_migration).is_err(),
                "accepted corruption {corruption}"
            );
            assert_eq!(catalog.metadata_with_prefix("").unwrap(), before);
        }
    }
}

#[test]
fn private_domain_merge_retains_only_its_own_replacements_and_deletions() {
    let current = registry(vec![
        domain("a", 9),
        domain("private", 7),
        domain("untouched", 4),
    ]);
    let committed = Arc::new(registry(vec![
        domain("a", 1),
        domain("peer", 3),
        domain("removed", 2),
        domain("untouched", 5),
    ]));
    let result = merge_private_records(&current, Arc::clone(&committed), |name| {
        Ok(matches!(
            name,
            "public.a" | "public.private" | "public.removed"
        ))
    })
    .unwrap();
    assert_eq!(
        result.keys().cloned().collect::<Vec<_>>(),
        [
            "public.a",
            "public.peer",
            "public.private",
            "public.untouched"
        ]
    );
    assert_eq!(result["public.a"].object_id, [9; 16]);
    assert_eq!(result["public.untouched"].object_id, [5; 16]);
    assert_eq!(committed["public.a"].object_id, [1; 16]);
    assert!(committed.contains_key("public.removed"));
}

#[test]
fn delayed_publication_preserves_peer_domains_and_rejects_a_changed_target() {
    let before = registry(vec![domain("removed", 1), domain("untouched", 2)]);
    let after = registry(vec![domain("untouched", 2)]);
    let current = registry(vec![
        domain("removed", 1),
        domain("untouched", 3),
        domain("peer", 4),
    ]);
    let merged = persistence::merge_changes(&before, &after, &current).unwrap();
    assert_eq!(
        merged.keys().cloned().collect::<Vec<_>>(),
        ["public.peer", "public.untouched"]
    );
    assert_eq!(merged["public.untouched"].object_id, [3; 16]);
    let replaced = registry(vec![domain("removed", 5), domain("untouched", 2)]);
    let error = persistence::merge_changes(&before, &after, &replaced).unwrap_err();
    assert_eq!(
        uqa_sql::catalog::errors::storage_error("domain", &error).sqlstate(),
        Some("40001")
    );
}
