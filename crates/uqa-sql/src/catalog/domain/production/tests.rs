//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{CreateDomain, DomainCheck, DomainNotNull, Expr, ObjectAclEntry};
use uqa_core::{catalog_role::RoleIdentity, memory::MemoryBudget, CancellationToken, Value};

fn domain(payload: &str) -> StoredDomain {
    StoredDomain {
        object_id: [9; 16],
        oid: 16000,
        array_oid: Some(16001),
        identity: RelationIdentity::new("public", "limited"),
        owner: RoleIdentity::BOOTSTRAP,
        definition: CreateDomain {
            name: "public.limited".into(),
            base: crate::ColumnType::Text,
            collation: Some("pg_catalog.C".into()),
            collation_oid: Some(950),
            default: Some(Expr::Literal(Value::Str(payload.into()))),
            not_null: Some(DomainNotNull {
                name: Some("required".into()),
                catalog_identity: None,
            }),
            checks: vec![DomainCheck {
                name: Some("permitted".into()),
                catalog_identity: None,
                expression: Expr::Literal(Value::Bool(true)),
                validated: true,
            }],
        },
        array_name: Some("_limited".into()),
        usage_acl: Some(vec![ObjectAclEntry {
            role: None,
            grantor: RoleIdentity::BOOTSTRAP,
            grant_option: false,
        }]),
    }
}

#[test]
fn selected_domain_copy_admits_exact_declaration_and_acl_ownership() {
    let source = domain(&"λ🙂".repeat(512));
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut copied = None;
    let measured = allocation_counter::measure(|| {
        copied = Some(source.clone_with_control(&control).unwrap());
    });
    let copied = copied.unwrap();
    assert_eq!(measured.bytes_current, memory.used() as i64);
    assert_eq!(copied.reserved_bytes(), memory.used());
    assert_eq!(
        serde_json::to_value(&*copied).unwrap(),
        serde_json::to_value(&source).unwrap()
    );
    drop(copied);
    assert_eq!(memory.used(), 0);
}

#[test]
fn selected_domain_copy_rejects_before_large_defaults_and_releases_partial_owners() {
    let source = domain(&"x".repeat(1 << 22));
    for cancelled in [None, Some(false), Some(true)] {
        let memory = MemoryBudget::new(if cancelled.is_some() {
            usize::MAX
        } else {
            4096
        });
        let sibling = memory.reserve(19).unwrap();
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if let Some(cancel_original) = cancelled {
            if cancel_original {
                original.cancel();
            } else {
                invoking.cancel();
            }
        }
        let control = ProductionControl::new(&memory, &original, &invoking);
        if cancelled.is_some() {
            assert!(control.check().is_err());
        }
        let measured = allocation_counter::measure(|| {
            assert_eq!(
                source.clone_with_control(&control).unwrap_err().sqlstate(),
                Some(if cancelled.is_some() {
                    "57014"
                } else {
                    "53200"
                })
            );
        });
        assert!(measured.bytes_total < 16 * 1024, "{measured:?}");
        assert_eq!(memory.used(), 19);
        drop(sibling);
        assert_eq!(memory.used(), 0);
    }
}
