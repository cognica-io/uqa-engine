//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn function(sql: &str, id: u8) -> Arc<SQLUserFunction> {
    let crate::Statement::CreateFunction(mut definition) = crate::compile(sql).unwrap().remove(0)
    else {
        panic!("function definition");
    };
    definition.object_id = Some([id; 16]);
    Arc::new(SQLUserFunction::new(
        *definition,
        crate::routines::RoutineBody::Bound(Arc::new(crate::routines::CompiledFunctionBody::SQL(
            Vec::new(),
        ))),
    ))
}

fn registry() -> RoutineRegistry {
    RoutineRegistry::from([
        ("first.f".into(), vec![function("CREATE FUNCTION first.f(integer) RETURNS integer IMMUTABLE LANGUAGE sql AS 'SELECT $1'", 1)]),
        ("second.f".into(), vec![
            function("CREATE FUNCTION second.f(integer) RETURNS integer VOLATILE LANGUAGE sql AS 'SELECT $1'", 2),
            function("CREATE FUNCTION second.f(text) RETURNS text STABLE LANGUAGE sql AS 'SELECT $1'", 3),
        ]),
    ])
}

#[test]
fn visible_routine_observation_preserves_first_schema_signatures_and_releases_admitted_scratch() {
    let registry = registry();
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut identities = [None; 2];
    let mut count = 0;
    visit_visible(
        &registry,
        ["first.f", "second.f", "first.f"],
        &control,
        |function| {
            identities[count] = function.def.object_id;
            count += 1;
            assert!(memory.used() > 0);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(identities, [Some([1; 16]), Some([3; 16])]);
    assert_eq!(memory.used(), 0);
    assert_eq!(
        routine_volatility_by_keys_with_control(&registry, ["first.f", "second.f"], &control)
            .unwrap(),
        Some(FunctionVolatility::Stable)
    );
    assert_eq!(
        routine_volatility_by_keys_with_control(&registry, ["second.f", "first.f"], &control)
            .unwrap(),
        Some(FunctionVolatility::Volatile)
    );
    assert_eq!(memory.used(), 0);
    let error = visit_visible(&registry, ["first.f", "second.f"], &control, |_| {
        Err(SQLError::Internal("visitor stopped".into()))
    })
    .unwrap_err();
    assert_eq!(error.to_string(), "internal error: visitor stopped");
    assert_eq!(memory.used(), 0);
}

#[test]
fn visible_routine_observation_rejects_before_visiting_and_bound_identity_needs_no_signature_copy()
{
    let registry = registry();
    let mut binding = crate::ast::FunctionBinding::polymorphic_builtin_syntax("coalesce");
    binding.builtin = false;
    binding.object_id = Some([2; 16]);
    for cancel_original in [None, Some(false), Some(true)] {
        let memory = MemoryBudget::new(0);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if let Some(cancel_original) = cancel_original {
            if cancel_original {
                original.cancel();
            } else {
                invoking.cancel();
            }
        }
        let control = ProductionControl::new(&memory, &original, &invoking);
        let error = visit_visible(&registry, ["first.f"], &control, |_| {
            panic!("unadmitted callback")
        })
        .unwrap_err();
        assert_eq!(
            error.sqlstate(),
            Some(if cancel_original.is_some() {
                "57014"
            } else {
                "53200"
            })
        );
        assert_eq!(memory.used(), 0);
        if cancel_original.is_none() {
            let allocations = allocation_counter::measure(|| {
                assert_eq!(
                    bound_routine_volatility_with_control(&registry, &binding, &control).unwrap(),
                    Some(FunctionVolatility::Volatile)
                );
            });
            assert_eq!(allocations.count_total, 0);
        }
    }
}
