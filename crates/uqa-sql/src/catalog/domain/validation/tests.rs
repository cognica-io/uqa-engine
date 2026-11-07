//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{ColumnType, ConstraintCatalogIdentity, CreateDomain, DomainCheck, DomainNotNull, Expr},
    catalog::{
        domain::validate_domain_registry,
        roles::{RoleDefinition, RoleIdentity},
        type_definitions::TypeDefinitions,
    },
};
use std::{
    cell::Cell,
    collections::BTreeMap,
    panic::{catch_unwind, AssertUnwindSafe},
};
use uqa_core::{
    memory::{MemoryBudget, OwnedMap},
    CancellationToken, Value,
};

fn domain(number: u32, checks: usize) -> StoredDomain {
    let object_id = u128::from(number + 1).to_le_bytes();
    let identity = RelationIdentity::new("public", format!("domain_{number}"));
    let constraint_identity = |index: usize| ConstraintCatalogIdentity {
        object_id: ((u128::from(number + 1) << 64) + index as u128 + 1).to_le_bytes(),
        oid: 20_000 + i64::from(number) * 1024 + index as i64,
    };
    StoredDomain {
        object_id,
        oid: domain_object_oid(&object_id),
        array_oid: None,
        owner: RoleIdentity::BOOTSTRAP,
        definition: CreateDomain {
            name: identity.qualified_name(),
            base: ColumnType::Integer,
            collation: None,
            collation_oid: None,
            default: None,
            not_null: Some(DomainNotNull {
                name: Some("required".into()),
                catalog_identity: Some(constraint_identity(0)),
            }),
            checks: (0..checks)
                .map(|index| DomainCheck {
                    name: Some(format!("check_{index}")),
                    catalog_identity: Some(constraint_identity(index + 1)),
                    expression: Expr::Literal(Value::Bool(true)),
                    validated: true,
                })
                .collect(),
        },
        identity,
        array_name: None,
        usage_acl: None,
    }
}

fn registry(count: u32, checks: usize) -> BTreeMap<String, StoredDomain> {
    (0..count)
        .map(|number| {
            let domain = domain(number, checks);
            (domain.identity.qualified_name(), domain)
        })
        .collect()
}

fn roles() -> RoleDefinitions {
    RoleDefinitions::try_from_iter([("uqa".into(), RoleDefinition::bootstrap())]).unwrap()
}

struct Observed<T> {
    registry: BTreeMap<String, T>,
    visits: Cell<usize>,
    panic_at: Option<usize>,
}
impl<T> TypeDefinitionView<T> for Observed<T> {
    fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a String, &'a T)>
    where
        T: 'a,
        String: 'a,
    {
        self.registry.iter().inspect(|_| {
            let next = self.visits.get() + 1;
            self.visits.set(next);
            assert_ne!(
                self.panic_at,
                Some(next),
                "injected domain iterator failure"
            );
        })
    }
    fn get(&self, name: &str) -> Option<&T> {
        self.registry.get(name)
    }
}

#[test]
fn domain_validation_indexes_role_incarnations_once_in_bounded_steps() {
    let mut source = registry(4, 1);
    let mut roles = Observed {
        registry: BTreeMap::new(),
        visits: Cell::new(0),
        panic_at: None,
    };
    for index in 0..4096_u32 {
        let mut role = RoleDefinition::bootstrap();
        role.name = format!("role_{index:04}");
        role.oid = 20_000 + i64::from(index);
        role.object_id = (u128::from(index) + 1).to_le_bytes();
        if index == 4095 {
            for domain in source.values_mut() {
                domain.owner = role.identity();
            }
        }
        roles.registry.insert(role.name.clone(), role);
    }
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(1 << 20);
    let mut job = begin_validation(
        &source,
        &roles,
        DomainValidationMode::Complete,
        ProductionControl::new(&memory, &token, &token),
    );
    assert_eq!(roles.visits.get(), 0);
    let mut steps = 0;
    loop {
        let before = roles.visits.get();
        let result = job.advance().unwrap();
        assert!(roles.visits.get() - before <= 1);
        steps += 1;
        if result == DomainValidationStep::Complete {
            break;
        }
        assert!(steps < 5000);
    }
    assert_eq!(roles.visits.get(), 4096);
    assert!(steps > 4096);
    assert_eq!(memory.used(), 0);
    drop(job);
    roles.visits.set(0);
    let original = CancellationToken::new();
    let mut interrupted = begin_validation(
        &source,
        &roles,
        DomainValidationMode::Complete,
        ProductionControl::new(&memory, &original, &token),
    );
    for _ in 0..33 {
        assert_eq!(
            interrupted.advance().unwrap(),
            DomainValidationStep::Pending
        );
    }
    let visited = roles.visits.get();
    assert!(visited > 0 && visited < 4096);
    assert!(memory.used() > 0);
    original.cancel();
    assert_eq!(
        interrupted
            .advance()
            .unwrap_err()
            .into_sql_error()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(roles.visits.get(), visited);
    assert_eq!(memory.used(), 0);
    drop(interrupted);
    let source = BTreeMap::new();
    roles.visits.set(0);
    let empty_memory = MemoryBudget::new(0);
    let mut empty = begin_validation(
        &source,
        &roles,
        DomainValidationMode::Complete,
        ProductionControl::new(&empty_memory, &token, &token),
    );
    while empty.advance().unwrap() != DomainValidationStep::Complete {}
    assert_eq!(roles.visits.get(), 0);
    assert_eq!(empty_memory.used(), 0);
}

#[test]
fn domain_validation_stops_role_visits_at_the_required_owner_prefix() {
    for last_owner in [0_u32, 2048] {
        let roles = Observed {
            registry: (0..4096_u32)
                .map(|index| {
                    let mut role = RoleDefinition::bootstrap();
                    role.name = format!("role_{index:04}");
                    role.oid = 20_000 + i64::from(index);
                    role.object_id = (u128::from(index) + 1).to_le_bytes();
                    (role.name.clone(), role)
                })
                .collect(),
            visits: Cell::new(0),
            panic_at: Some(last_owner as usize + 2),
        };
        let mut source = registry(3, 1);
        for (index, domain) in source.values_mut().enumerate() {
            let owner = if index == 1 { last_owner } else { 0 };
            domain.owner = roles.registry[&format!("role_{owner:04}")].identity();
        }
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut job = begin_validation(
            &source,
            &roles,
            DomainValidationMode::Complete,
            ProductionControl::new(&memory, &token, &token),
        );
        loop {
            let before = roles.visits.get();
            let result = job.advance().unwrap();
            assert!(roles.visits.get() - before <= 1);
            if result == DomainValidationStep::Complete {
                break;
            }
        }
        assert_eq!(roles.visits.get(), last_owner as usize + 1);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn domain_validation_admits_each_step_and_retires_complete_scratch() {
    let source = Observed {
        registry: registry(3, 256),
        visits: Cell::new(0),
        panic_at: None,
    };
    let roles = roles();
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut job = None;
    let allocation = allocation_counter::measure(|| {
        job = Some(begin_validation(
            &source,
            &roles,
            DomainValidationMode::Complete,
            ProductionControl::new(&memory, &token, &token),
        ));
    });
    assert_eq!(allocation.count_total, 0);
    assert_eq!(source.visits.get(), 0);
    assert_eq!(memory.used(), 0);
    let mut job = job.unwrap();
    let maximum_growth =
        3 * OwnedMap::<&str, ()>::entry_bytes().max(OwnedMap::<[u8; 16], ()>::entry_bytes());
    let mut steps = 0;
    loop {
        let before = memory.used();
        let visits = source.visits.get();
        let result = job.advance().unwrap();
        assert!(source.visits.get() - visits <= 1);
        assert!(memory.used().saturating_sub(before) <= maximum_growth);
        steps += 1;
        if result == DomainValidationStep::Complete {
            break;
        }
        assert!(steps < 10_000);
    }
    assert!(steps > 3 * 256 * 3);
    assert_eq!(source.visits.get(), 6);
    assert_eq!(memory.used(), 0);
    assert!(memory.peak() > 0);
    token.cancel();
    assert_eq!(job.advance().unwrap(), DomainValidationStep::Complete);
    assert_eq!(source.visits.get(), 6);
}

#[test]
fn domain_validation_failure_retires_scratch_before_retry_or_drop() {
    let source = registry(4, 16);
    let roles = roles();
    let token = CancellationToken::new();
    let ample = MemoryBudget::new(1 << 20);
    let mut job = begin_validation(
        &source,
        &roles,
        DomainValidationMode::Complete,
        ProductionControl::new(&ample, &token, &token),
    );
    while job.advance().unwrap() != DomainValidationStep::Complete {}
    let peak = ample.peak();
    for limit in [0, 1, 128, peak - 1, peak] {
        let memory = MemoryBudget::new(limit);
        let mut job = begin_validation(
            &source,
            &roles,
            DomainValidationMode::Complete,
            ProductionControl::new(&memory, &token, &token),
        );
        let outcome = loop {
            match job.advance() {
                Ok(DomainValidationStep::Pending) => (),
                outcome => break outcome,
            }
        };
        if limit == peak {
            assert_eq!(outcome.unwrap(), DomainValidationStep::Complete);
        } else {
            assert_eq!(
                outcome.unwrap_err().into_sql_error().sqlstate(),
                Some("53200")
            );
            assert!(job.advance().unwrap_err().to_string().contains("finished"));
        }
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn domain_validation_preserves_both_cancellation_owners() {
    let source = registry(2, 16);
    let roles = roles();
    for original_cancelled in [false, true] {
        let memory = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let mut job = begin_validation(
            &source,
            &roles,
            DomainValidationMode::Complete,
            ProductionControl::new(&memory, &original, &invoking),
        );
        for _ in 0..5 {
            assert_eq!(job.advance().unwrap(), DomainValidationStep::Pending);
        }
        assert!(memory.used() > 0);
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert_eq!(
            job.advance().unwrap_err().into_sql_error().sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), 0);
        assert!(job.advance().unwrap_err().to_string().contains("finished"));
    }
}

#[test]
fn domain_validation_is_terminal_after_iterator_unwind() {
    let source = Observed {
        registry: registry(2, 4),
        visits: Cell::new(0),
        panic_at: Some(2),
    };
    let roles = roles();
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut job = begin_validation(
        &source,
        &roles,
        DomainValidationMode::Complete,
        ProductionControl::new(&memory, &token, &token),
    );
    assert!(catch_unwind(AssertUnwindSafe(|| {
        while job.advance().unwrap() != DomainValidationStep::Complete {}
    }))
    .is_err());
    assert_eq!(source.visits.get(), 2);
    assert_eq!(memory.used(), 0);
    assert!(job.advance().unwrap_err().to_string().contains("finished"));
    assert_eq!(source.visits.get(), 2);
}

#[test]
fn domain_validation_keeps_local_global_and_legacy_failure_order() {
    let roles = roles();
    let mut source = registry(2, 2);
    let duplicate = source["public.domain_0"].definition.checks[0].catalog_identity;
    let second = source.get_mut("public.domain_1").unwrap();
    second.definition.checks[0].catalog_identity = duplicate;
    second.definition.checks[1].name = second.definition.checks[0].name.clone();
    assert_eq!(
        validate_domain_registry(&source, &roles).unwrap_err(),
        "invalid or duplicate domain constraint name"
    );
    source.get_mut("public.domain_1").unwrap().definition.checks[1].name =
        Some("independent".into());
    assert_eq!(
        validate_domain_registry(&source, &roles).unwrap_err(),
        "duplicate domain constraint catalog identity for `public.domain_1`"
    );
    let mut source = registry(2, 2);
    source
        .get_mut("public.domain_0")
        .unwrap()
        .definition
        .not_null
        .as_mut()
        .unwrap()
        .name = None;
    source.get_mut("public.domain_1").unwrap().object_id = [0; 16];
    assert_eq!(
        validate_domain_registry(&source, &roles).unwrap_err(),
        "invalid or duplicate domain identity for `public.domain_1`"
    );
    source.remove("public.domain_1");
    super::super::validate_domain_definitions(&source, &roles).unwrap();
    assert_eq!(
        validate_domain_registry(&source, &roles).unwrap_err(),
        "domain constraint has no durable name"
    );
}

#[test]
fn domain_validation_accepts_captured_native_roots_and_exact_quoted_names() {
    let mut source = TypeDefinitions::new();
    let mut value = domain(0, 1);
    value.identity = RelationIdentity::new("a.b", "x\"y");
    value.definition.name = value.identity.qualified_name();
    source
        .insert(value.identity.qualified_name(), value)
        .unwrap();
    let roles = roles();
    let memory = MemoryBudget::new(1 << 20);
    let source_memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let captured = source
        .capture(&ProductionControl::new(&source_memory, &token, &token))
        .unwrap();
    source.clear();
    drop(source);
    let retained = source_memory.used();
    let mut job = begin_validation(
        &*captured,
        &roles,
        DomainValidationMode::Complete,
        ProductionControl::new(&memory, &token, &token),
    );
    while job.advance().unwrap() != DomainValidationStep::Complete {}
    assert_eq!(memory.used(), 0);
    assert_eq!(source_memory.used(), retained);
    drop(job);
    drop(captured);
    assert_eq!(source_memory.used(), 0);
}
