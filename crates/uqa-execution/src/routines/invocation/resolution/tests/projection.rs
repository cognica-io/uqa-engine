//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::routines::invocation::has_described_user_function;

struct Lookup<'a>(&'a Catalog);

impl uqa_sql::FunctionTypeResolver for Lookup<'_> {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        panic!("presence inspection cannot resolve a routine signature")
    }
}

impl uqa_sql::routines::RoutineResolution for Lookup<'_> {
    fn lookup_bound_sql_functions_by_binding(
        &self,
        binding: &FunctionBinding,
    ) -> Option<Vec<Arc<SQLUserFunction>>> {
        self.0.lookup_bound_sql_functions_by_binding(binding)
    }
}

#[test]
fn projection_presence_and_invocation_share_successful_bound_metadata() {
    let catalog = Catalog::new();
    let binding = catalog.binding();
    let descriptions = RoutineCallDescriptions::default();
    let scope = || Some(catalog.scope.clone());
    let budget = MemoryBudget::new(8192);
    let token = CancellationToken::new();
    let retained = RoutineCallRetention {
        descriptions: &descriptions,
        scope: &scope,
        control: ProductionControl::new(&budget, &token, &token),
    };
    for value in 0..512 {
        assert!(has_described_user_function(
            &Lookup(&catalog),
            &binding.name,
            Some(&binding),
            retained
        )
        .unwrap());
        let selected = resolve_bound_inputs(
            &catalog,
            &RoutineOverloadContext { catalog: &catalog },
            Some(retained),
            &binding,
            &[(None, Value::Int(value))],
        )
        .unwrap()
        .unwrap();
        assert!(Arc::ptr_eq(&selected.function, &catalog.function));
        assert_eq!(selected.bound, vec![Value::Int(value)]);
    }
    assert_eq!(
        catalog.lookups.load(Ordering::Relaxed),
        2,
        "only the first presence and invocation resolve the routine"
    );
    assert_eq!(catalog.sources.load(Ordering::Relaxed), 1);
    assert_eq!(catalog.targets.load(Ordering::Relaxed), 1);
    let changed = || Some(RoutineDescriptionScope::default());
    assert!(has_described_user_function(
        &Lookup(&catalog),
        &binding.name,
        Some(&binding),
        RoutineCallRetention {
            scope: &changed,
            ..retained
        }
    )
    .unwrap());
    assert_eq!(catalog.lookups.load(Ordering::Relaxed), 3);
    let mut builtin = binding.clone();
    builtin.builtin = true;
    assert!(!has_described_user_function(
        &Lookup(&catalog),
        &binding.name,
        Some(&builtin),
        retained
    )
    .unwrap());
    assert_eq!(catalog.lookups.load(Ordering::Relaxed), 3);
    drop(descriptions);
    assert_eq!(budget.used(), 0);
}

#[test]
fn projection_presence_does_not_pin_a_removed_routine() {
    let catalog = Catalog::new();
    let binding = catalog.binding();
    let descriptions = RoutineCallDescriptions::default();
    let temporary = Arc::new(SQLUserFunction::new(
        catalog.function.def.clone(),
        RoutineBody::Source,
    ));
    let scope = || Some(catalog.scope.clone());
    let control = ProductionControl::uncontrolled();
    descriptions
        .remember(
            &binding,
            &temporary,
            &[Some(ColumnType::Integer)],
            catalog.scope.clone(),
            &control,
        )
        .unwrap();
    let retained = RoutineCallRetention {
        descriptions: &descriptions,
        scope: &scope,
        control,
    };
    assert!(has_described_user_function(
        &Lookup(&catalog),
        &binding.name,
        Some(&binding),
        retained
    )
    .unwrap());
    assert_eq!(catalog.lookups.load(Ordering::Relaxed), 0);
    drop(temporary);
    assert!(has_described_user_function(
        &Lookup(&catalog),
        &binding.name,
        Some(&binding),
        retained
    )
    .unwrap());
    assert_eq!(
        catalog.lookups.load(Ordering::Relaxed),
        1,
        "expired metadata must return to the live lookup"
    );
}
