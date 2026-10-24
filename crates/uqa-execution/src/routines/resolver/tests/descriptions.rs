//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::routines::descriptions::ProceduralDescriptions;
use std::{cell::RefCell, sync::Arc};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};
use uqa_sql::{
    ast::{FunctionBinding, RoutineInvocationBinding, RoutineVariadicMode},
    plpgsql::{parse_function, PLpgSQLFunction},
    routines::{
        invocation::descriptions::{
            RoutineCallDescriptions, RoutineConversionEnvironment, RoutineDescriptionScope,
        },
        RoutineBody, SQLUserFunction,
    },
};

fn fixture() -> (Arc<SQLUserFunction>, FunctionBinding, PLpgSQLFunction) {
    let uqa_sql::Statement::CreateFunction(mut definition) = uqa_sql::compile(
        "CREATE FUNCTION public.procedural_read() RETURNS smallint LANGUAGE plpgsql AS $$ DECLARE v smallint; BEGIN RETURN v; END $$",
    ).unwrap().remove(0) else { panic!("routine") };
    definition.object_id = Some([12; 16]);
    let mut parsed = parse_function(&definition).unwrap();
    for datum in &mut parsed.datums {
        if let PLpgSQLDatum::Var(variable) = datum {
            variable.type_name = ColumnType::from_sql_name(&variable.type_name)
                .unwrap()
                .catalog_name();
        }
    }
    let function = Arc::new(SQLUserFunction::new(*definition, RoutineBody::Source));
    let binding = FunctionBinding {
        name: function.def.name.clone(),
        object_id: function.def.object_id,
        argument_types: Vec::new(),
        builtin: false,
        dispatch: None,
        invocation: Some(Box::new(RoutineInvocationBinding {
            argument_positions: Vec::new(),
            argument_targets: Vec::new(),
            argument_sources: Vec::new(),
            parameter_types: Vec::new(),
            return_type: Some("pg_catalog.int2".into()),
            variadic_mode: RoutineVariadicMode::None,
        })),
        composite_field: None,
        resolution_error: None,
        input_collation: None,
    };
    (function, binding, parsed)
}

#[test]
fn native_procedural_parameters_reuse_declarations_but_read_current_values() {
    let types = Types::default();
    let (function, binding, parsed) = fixture();
    let state = RoutineCallDescriptions::default();
    let initial = RoutineDescriptionScope::default();
    let current = RefCell::new(initial.clone());
    let scope = || Some(current.borrow().clone());
    let budget = MemoryBudget::new(8192);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    state
        .remember(&binding, &function, &[], initial.clone(), &control)
        .unwrap();
    let environment = RoutineConversionEnvironment {
        values: &types,
        scope: &scope,
        control,
    };
    let description = state
        .get(&binding, &initial)
        .unwrap()
        .procedural_body(&parsed, environment)
        .unwrap()
        .unwrap();
    let index = parsed
        .datums
        .iter()
        .position(|datum| datum.name() == Some("v"))
        .unwrap();
    let bindings = HashMap::from([("v".into(), vec![index])]);
    let schemas = HashMap::new();
    let mut values = vec![Value::Null; parsed.datums.len()];
    let reference = PLpgSQLVariableReference::Name("v".into());
    for iteration in 0..514 {
        if iteration == 512 {
            *current.borrow_mut() = RoutineDescriptionScope::default();
        }
        values[index] = if iteration % 3 == 0 {
            Value::Null
        } else {
            Value::Int(iteration)
        };
        let mut resolver = DatumResolver {
            descriptions: Some(ProceduralDescriptions {
                description,
                environment,
            }),
            types: &types,
            datums: &parsed.datums,
            values: &values,
            record_schemas: &schemas,
            bindings: &bindings,
            error: None,
            param_count: 0,
        };
        let parameter = reference.read(&mut resolver).unwrap();
        assert_eq!(parameter.scalar_value(), Some(&values[index]));
        assert_eq!(
            parameter.declared_scalar_type(),
            Some(&ColumnType::SmallInteger)
        );
        if iteration == 511 {
            assert_eq!(types.resolutions.get(), 1);
        }
    }
    assert_eq!(
        types.resolutions.get(),
        3,
        "scope change requires live declaration reads"
    );
    assert!(budget.used() > 0 && budget.used() <= 8192);
    drop(state);
    assert_eq!(budget.used(), 0);
}
