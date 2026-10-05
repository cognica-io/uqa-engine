//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::routines::{
    routine_signature_types, CompiledFunctionBody, RoutineBody, SQLUserFunction,
};
use std::sync::Arc;

fn routine(sql: &str, identity: u8) -> Arc<SQLUserFunction> {
    let crate::Statement::CreateFunction(mut definition) = crate::compile(sql).unwrap().remove(0)
    else {
        unreachable!("CREATE FUNCTION fixture")
    };
    definition.object_id = Some([identity; 16]);
    Arc::new(SQLUserFunction::new(
        *definition,
        RoutineBody::Bound(Arc::new(CompiledFunctionBody::SQL(Vec::new()))),
    ))
}

fn register(registry: &mut RoutineRegistry, routine: &Arc<SQLUserFunction>) {
    registry
        .entry(routine.def.name.clone())
        .or_default()
        .push(Arc::clone(routine));
}

fn target(routine: &SQLUserFunction) -> RoutineDropTarget {
    RoutineDropTarget {
        object_id: routine.def.object_id,
        name: routine.def.name.clone(),
        argument_types: routine_signature_types(&routine.def),
        is_procedure: routine.def.is_procedure,
    }
}

fn identities(registry: &RoutineRegistry) -> Vec<Option<[u8; 16]>> {
    registry
        .values()
        .flatten()
        .map(|routine| routine.def.object_id)
        .collect()
}

#[test]
fn an_overload_registered_after_the_targets_were_resolved_is_retained() {
    let resolved = routine(
        "CREATE FUNCTION public.f() RETURNS INTEGER LANGUAGE SQL AS 'SELECT 1'",
        1,
    );
    let mut registry = RoutineRegistry::new();
    register(&mut registry, &resolved);
    let targets = [target(&resolved)];
    register(
        &mut registry,
        &routine(
            "CREATE FUNCTION public.f(value INTEGER) RETURNS INTEGER LANGUAGE SQL AS 'SELECT $1'",
            2,
        ),
    );
    remove_routine_registry_targets(&mut registry, &targets).unwrap();
    assert_eq!(identities(&registry), [Some([2; 16])]);
}

#[test]
fn a_removal_whose_target_disappeared_removes_nothing() {
    let first = routine(
        "CREATE FUNCTION public.first() RETURNS INTEGER LANGUAGE SQL AS 'SELECT 1'",
        1,
    );
    let second = routine(
        "CREATE FUNCTION public.second() RETURNS INTEGER LANGUAGE SQL AS 'SELECT 2'",
        2,
    );
    let mut registry = RoutineRegistry::new();
    register(&mut registry, &first);
    let targets = [target(&first), target(&second)];
    let error = remove_routine_registry_targets(&mut registry, &targets).unwrap_err();
    assert!(matches!(error, SQLError::Internal(_)), "{error}");
    assert_eq!(identities(&registry), [Some([1; 16])]);
}

#[test]
fn a_routine_recreated_under_another_identity_is_not_removed() {
    let resolved = routine(
        "CREATE FUNCTION public.f() RETURNS INTEGER LANGUAGE SQL AS 'SELECT 1'",
        1,
    );
    let targets = [target(&resolved)];
    let mut registry = RoutineRegistry::new();
    register(
        &mut registry,
        &routine(
            "CREATE FUNCTION public.f() RETURNS INTEGER LANGUAGE SQL AS 'SELECT 1'",
            3,
        ),
    );
    let error = remove_routine_registry_targets(&mut registry, &targets).unwrap_err();
    assert!(matches!(error, SQLError::Internal(_)), "{error}");
    assert_eq!(identities(&registry), [Some([3; 16])]);
}
