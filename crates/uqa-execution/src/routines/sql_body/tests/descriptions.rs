//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::definition;
use crate::routines::{descriptions::RoutineBodyDescriptions, sql_body::body_parameters};
use std::{cell::Cell, sync::Arc};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken, Value,
};
use uqa_sql::{
    assignment::{routines::RoutineValueContext, AssignmentContext},
    ast::{FunctionBinding, RoutineInvocationBinding, RoutineVariadicMode},
    catalog::domain::{DomainCatalog, StoredDomain},
    expr::EngineHook,
    routines::{
        invocation::descriptions::{
            RoutineCallDescriptions, RoutineConversionEnvironment, RoutineDescriptionScope,
        },
        RoutineBody, SQLUserFunction,
    },
    ColumnType, SQLError,
};

#[derive(Default)]
struct Inputs {
    resolutions: Cell<usize>,
}

impl EngineHook for Inputs {
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        unreachable!()
    }
}

impl DomainCatalog for Inputs {
    fn domain_by_oid(&self, _: u32) -> Option<StoredDomain> {
        None
    }
}

impl AssignmentContext for Inputs {
    fn evaluate_domain_check(
        &self,
        _: &uqa_sql::ast::Expr,
        _: &dyn uqa_sql::expr::RowLookup,
        _: &uqa_sql::RowSchema,
    ) -> Result<Value, SQLError> {
        unreachable!()
    }
}

impl RoutineValueContext for Inputs {
    fn catalog_column_type(&self, name: &str) -> Option<ColumnType> {
        self.resolutions.set(self.resolutions.get() + 1);
        assert_eq!(name, "public.input_value");
        Some(ColumnType::Integer)
    }
}

#[test]
fn repeated_sql_body_inputs_keep_all_parameter_positions_and_current_values() {
    let mut definition = definition(
        "CREATE FUNCTION public.body_inputs(OUT label text, IN value integer) LANGUAGE sql AS 'SELECT value::text'",
    );
    definition.object_id = Some([11; 16]);
    definition.params[0].type_name = "pg_catalog.text".into();
    definition.params[1].type_name = "public.input_value".into();
    let function = Arc::new(SQLUserFunction::new(definition, RoutineBody::Source));
    let binding = FunctionBinding {
        name: function.def.name.clone(),
        object_id: function.def.object_id,
        argument_types: vec!["public.input_value".into()],
        builtin: false,
        dispatch: None,
        invocation: Some(Box::new(RoutineInvocationBinding {
            argument_positions: vec![1],
            argument_targets: vec!["public.input_value".into()],
            argument_sources: vec![Some("pg_catalog.int4".into())],
            parameter_types: vec!["pg_catalog.text".into(), "public.input_value".into()],
            return_type: Some("pg_catalog.text".into()),
            variadic_mode: RoutineVariadicMode::None,
        })),
        composite_field: None,
        resolution_error: None,
        input_collation: None,
    };
    for retain in [false, true] {
        let inputs = Inputs::default();
        let budget = MemoryBudget::new(4096);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let state = RoutineCallDescriptions::default();
        let scope = RoutineDescriptionScope::default();
        let current = || Some(scope.clone());
        state
            .remember(
                &binding,
                &function,
                &[Some(ColumnType::Integer)],
                scope.clone(),
                &control,
            )
            .unwrap();
        let descriptions = retain.then(|| RoutineBodyDescriptions {
            description: state.get(&binding, &scope).unwrap(),
            environment: RoutineConversionEnvironment {
                values: &inputs,
                scope: &current,
                control,
            },
        });
        for index in 0..512 {
            let params =
                body_parameters(&inputs, &function.def, &[Value::Int(index)], descriptions)
                    .unwrap();
            assert_eq!(params.len(), 1);
            assert_eq!(params[0].declared_scalar_type(), Some(&ColumnType::Integer));
            assert_eq!(params[0].scalar_value(), Some(&Value::Int(index)));
        }
        assert_eq!(inputs.resolutions.get(), if retain { 1 } else { 512 });
        drop(state);
        assert_eq!(budget.used(), 0);
    }
}
