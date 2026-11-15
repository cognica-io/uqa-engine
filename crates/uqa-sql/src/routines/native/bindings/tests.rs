//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{ColumnDef, CreateFunction, Expr, Statement},
    binding::fixture::{empty_binding_context, NoRoutines},
    catalog::{
        type_definitions::TypeDefinitions,
        type_references::{TypeReferenceInput, TypeReferences},
    },
    routines::native::NativeRoutineReferences,
    ColumnType,
};
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};

struct Types;

impl RoutineTypeCatalog for Types {
    fn try_describe_table(&self, _: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        Ok(None)
    }
    fn resolve_catalog_column_type(&self, name: &str) -> Option<ColumnType> {
        ColumnType::from_sql_name(name).ok()
    }
    fn resolve_catalog_column_type_name(&self, name: &str) -> Result<ColumnType, SQLError> {
        ColumnType::from_sql_name(name)
    }
    fn resolve_catalog_user_type_by_oid(&self, _: u32) -> Option<ColumnType> {
        None
    }
    fn require_type_usage(&self, _: &ColumnType) -> Result<(), SQLError> {
        Ok(())
    }
    fn format_type(&self, ty: &ColumnType) -> Result<String, SQLError> {
        Ok(ty.regtype_name())
    }
}

fn declaration() -> CreateFunction {
    let Statement::CreateFunction(mut definition) = crate::compile(
        "CREATE FUNCTION public.saved(n integer) RETURNS integer LANGUAGE SQL AS 'uncompiled missing($77)'",
    ).unwrap().remove(0) else { panic!("routine declaration") };
    definition.object_id = Some([1; 16]);
    *definition
}

fn captured(syntax: &str, compiled: &str) -> SQLUserFunction {
    let mut definition = declaration();
    definition.body = FunctionBody::Statements(crate::compile(syntax).unwrap());
    let plans = crate::compile(compiled)
        .unwrap()
        .into_iter()
        .map(UnifiedPlan::lower)
        .collect();
    SQLUserFunction::new(
        definition,
        RoutineBody::Bound(CompiledFunctionBody::SQL(plans).shared()),
    )
}

fn image(routine: &SQLUserFunction) -> serde_json::Value {
    let plans = match &routine.body {
        RoutineBody::Source => None,
        RoutineBody::Bound(body) => match &***body {
            CompiledFunctionBody::SQL(plans) => Some(plans),
            CompiledFunctionBody::PLpgSQL(_) => unreachable!(),
        },
    };
    serde_json::to_value((&routine.def, plans)).unwrap()
}

fn validate(
    routine: &SQLUserFunction,
    memory: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Result<(), SQLError> {
    let before = image(routine);
    let control = ProductionControl::new(memory, original, invoking);
    let result = (|| {
        super::super::validate_captured_body(routine, &control)?;
        let binding = empty_binding_context().copy_with_control(&control)?;
        let references =
            NativeRoutineReferences::new(std::iter::once(&routine.def), memory, original)?;
        let input = TypeReferenceInput {
            domains: &TypeDefinitions::default(),
            enums: &TypeDefinitions::default(),
            composites: &TypeDefinitions::default(),
            ranges: &TypeDefinitions::default(),
            shells: &TypeDefinitions::default(),
        };
        let types = TypeReferences::new(input, std::iter::empty(), memory, original)?;
        let context = NativeScalarContext {
            subject: "routine default",
            routines: &NoRoutines,
            references: &references,
            types: &types,
            binding: &binding,
        };
        validate_captured_bindings(routine, &Types, &context, &control)
    })();
    assert_eq!(image(routine), before);
    assert_eq!(memory.used(), 0);
    result
}

fn check(routine: &SQLUserFunction) -> Result<(), SQLError> {
    validate(
        routine,
        &MemoryBudget::new(2 << 20),
        &CancellationToken::new(),
        &CancellationToken::new(),
    )
}

#[test]
fn captured_routine_binding_checks_both_original_and_executable_results() {
    for bad in [
        "SELECT true",
        "SELECT missing_column",
        "SELECT $1 + true",
        "SELECT $1, $1",
    ] {
        for routine in [captured(bad, "SELECT $1"), captured("SELECT n", bad)] {
            assert!(check(&routine).is_err(), "{bad}");
        }
    }
    assert!(check(&captured("", "")).is_err());
    let mut procedure = captured("", "");
    procedure.def.is_procedure = true;
    procedure.def.returns = crate::ast::FunctionReturns::None;
    check(&procedure).unwrap();
}

#[test]
fn captured_routine_binding_preserves_parameter_names_ctes_windows_and_lazy_scalars() {
    for (syntax, plan) in [
        ("SELECT n", "SELECT $1"),
        ("SELECT saved.n + 1", "SELECT $1 + 1"),
        ("SELECT 10 / n", "SELECT 10 / $1"),
        ("SELECT 10 / 0", "SELECT 10 / 0"),
        (
            "WITH x AS (SELECT n AS id) SELECT (SELECT id FROM x)",
            "WITH x AS (SELECT $1 AS id) SELECT (SELECT id FROM x)",
        ),
        (
            "SELECT sum(n) OVER (ORDER BY n)",
            "SELECT sum($1) OVER (ORDER BY $1)",
        ),
        (
            "SELECT CASE WHEN n=0 THEN 7 ELSE 10/n END",
            "SELECT CASE WHEN $1=0 THEN 7 ELSE 10/$1 END",
        ),
    ] {
        check(&captured(syntax, plan)).unwrap_or_else(|error| panic!("{syntax}: {error}"));
    }
}

#[test]
fn captured_routine_defaults_require_scalar_compatible_inputs_without_body_compilation() {
    let mut routine = SQLUserFunction::new(declaration(), RoutineBody::Source);
    for sql in [
        "SELECT true",
        "SELECT missing_column",
        "SELECT sum(1)",
        "SELECT (SELECT 1)",
        "SELECT $1",
    ] {
        let Statement::Select(select) = crate::compile(sql).unwrap().remove(0) else {
            panic!("query")
        };
        let expr = &select.projections[0].expr;
        routine.def.params[0].default = Some(expr.clone());
        assert!(check(&routine).is_err(), "{sql}");
    }
    routine.def.params[0].default = Some(Expr::Literal(Value::Int(7)));
    check(&routine).unwrap();
    routine.def.params[0].default = None;
    for language in ["sql", "plpgsql"] {
        routine.def.language = language.into();
        check(&routine).unwrap();
    }
}

#[test]
fn captured_routine_binding_admission_and_cancellation_retire_original_scratch() {
    let routine = captured("SELECT (SELECT n + 1)", "SELECT (SELECT $1 + 1)");
    let mut denied = 0;
    let mut passed = 0;
    for limit in [0, 64, 1024, 2 << 20] {
        let result = validate(
            &routine,
            &MemoryBudget::new(limit),
            &CancellationToken::new(),
            &CancellationToken::new(),
        );
        match result {
            Ok(()) => passed += 1,
            Err(error) => {
                assert_eq!(error.sqlstate(), Some("53200"));
                denied += 1;
            }
        }
    }
    assert!(denied > 0 && passed > 0);
    for cancel_original in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let error =
            validate(&routine, &MemoryBudget::new(2 << 20), &original, &invoking).unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
    }
}
