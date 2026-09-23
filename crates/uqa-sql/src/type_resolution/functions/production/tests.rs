//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn resolve(
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    column: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    let mut infer = |expression: &ScalarExpr| match expression {
        ScalarExpr::Column(_) => copy(Some(column), control),
        ScalarExpr::Literal(value) => {
            super::super::super::common::value_type_with_control(value, control)
        }
        _ => Ok(None),
    };
    builtin_function_type_with_control(
        FunctionTypeCall {
            name,
            binding,
            args,
        },
        &[],
        &[],
        None,
        &mut infer,
        control,
    )
}

fn domain() -> ColumnType {
    ColumnType::Domain {
        schema: "app".into(),
        name: "bounded_text".into(),
        oid: 99_999,
        base: Box::new(ColumnType::Varchar(Some(17))),
    }
}

#[test]
fn function_results_retain_nested_type_payloads_and_release_inference_scratch() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let field = ScalarExpr::Column("value".into());
    let array_type = ColumnType::Array(Box::new(domain()));
    for (name, ty, expected) in [
        ("min", domain(), domain()),
        ("array_agg", domain(), array_type.clone()),
        ("array_cat", array_type.clone(), array_type.clone()),
        ("unnest", array_type, domain()),
        ("abs", domain(), ColumnType::Varchar(Some(17))),
        (
            "PG_CATALOG.ARRAY_REVERSE",
            ColumnType::Array(Box::new(ColumnType::Json)),
            ColumnType::Array(Box::new(ColumnType::Json)),
        ),
    ] {
        let output = resolve(name, None, std::slice::from_ref(&field), &ty, &control)
            .unwrap()
            .unwrap();
        assert_eq!(*output, expected, "{name}");
        assert_eq!(budget.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn function_type_dispatch_preserves_unknown_common_types_fixed_overloads_and_binding_errors() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (name, args, expected) in [
        (
            "coalesce",
            vec![
                ScalarExpr::Literal(Value::Str("3".into())),
                ScalarExpr::Literal(Value::Int(4)),
            ],
            ColumnType::Integer,
        ),
        (
            "length",
            vec![ScalarExpr::Literal(Value::Str("text".into()))],
            ColumnType::Integer,
        ),
        (
            "array_positions",
            vec![ScalarExpr::Literal(Value::Null)],
            ColumnType::Array(Box::new(ColumnType::Integer)),
        ),
        (
            "concat_op",
            vec![
                ScalarExpr::Literal(Value::Null),
                ScalarExpr::Literal(Value::Null),
            ],
            ColumnType::Text,
        ),
    ] {
        let output = resolve(name, None, &args, &ColumnType::Integer, &control)
            .unwrap()
            .unwrap();
        assert_eq!(*output, expected);
        assert_eq!(budget.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
    let binding = FunctionBinding {
        object_id: Some([1; 16]),
        name: "app.array_sort".into(),
        argument_types: Vec::new(),
        builtin: false,
        dispatch: None,
        invocation: None,
        resolution_error: None,
    };
    let args = [ScalarExpr::Literal(Value::Null)];
    let error = resolve(
        "array_sort",
        Some(&binding),
        &args,
        &ColumnType::Integer,
        &control,
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42804"));
    assert_eq!(budget.used(), 0);
    let ordinary = super::super::builtin_function_type_inner(
        "array_sort",
        Some(&binding),
        &args,
        &[],
        &crate::RowSchema::default(),
        &[],
        None,
    )
    .unwrap_err();
    assert_eq!(ordinary.sqlstate(), error.sqlstate());
    assert_eq!(ordinary.to_string(), error.to_string());
}

#[test]
fn inference_resource_errors_remain_typed_and_release_partial_type_buffers() {
    let budget = MemoryBudget::new(4096);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let field = ScalarExpr::Column("value".into());
    let oversized = ColumnType::Named("x".repeat(8192));
    let error = resolve(
        "min",
        None,
        std::slice::from_ref(&field),
        &oversized,
        &control,
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(budget.used(), 0);
    for sqlstate in ["42883", "53200", "57014"] {
        let mut infer = |_: &ScalarExpr| {
            Err(SQLError::Routine {
                sqlstate: sqlstate.into(),
                message: "fixture type failure".into(),
            })
        };
        let error = builtin_function_type_with_control(
            FunctionTypeCall {
                name: "min",
                binding: None,
                args: std::slice::from_ref(&field),
            },
            &[],
            &[],
            None,
            &mut infer,
            &control,
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some(sqlstate));
        assert_eq!(budget.used(), 0);
    }
    for original_cancelled in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        let error = resolve(
            "min",
            None,
            std::slice::from_ref(&field),
            &ColumnType::Integer,
            &control,
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
    }
}

struct Resolver {
    calls: std::sync::Mutex<Vec<String>>,
    expected_parameter: Option<ColumnType>,
}

impl FunctionTypeResolver for Resolver {
    fn has_untyped_function(&self, name: &str) -> bool {
        self.calls.lock().unwrap().push(format!("lookup:{name}"));
        false
    }

    fn resolve_function_type(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        names: &[Option<String>],
        types: &[Option<ColumnType>],
        explicit: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        self.calls.lock().unwrap().push(format!("return:{name}"));
        assert!(binding.is_none());
        assert_eq!(names, &[None]);
        assert_eq!(types, std::slice::from_ref(&self.expected_parameter));
        assert!(!explicit);
        Ok(Some(ColumnType::Array(Box::new(ColumnType::Text))))
    }
}

#[test]
fn ordinary_external_resolution_preserves_callback_order_and_typed_parameter_identity() {
    for (parameter, expected_parameter) in [
        (SQLParam::Scalar(Value::Str("x".into())), None),
        (
            SQLParam::typed_scalar(Value::Str("x".into()), ColumnType::Text),
            Some(ColumnType::Text),
        ),
    ] {
        let resolver = Resolver {
            calls: std::sync::Mutex::new(Vec::new()),
            expected_parameter,
        };
        let result = super::super::builtin_function_type_inner(
            "custom.return_values",
            None,
            &[ScalarExpr::Param(1)],
            &[],
            &crate::RowSchema::default(),
            &[parameter],
            Some(&resolver),
        )
        .unwrap();
        assert_eq!(result, Some(ColumnType::Array(Box::new(ColumnType::Text))));
        assert_eq!(
            *resolver.calls.lock().unwrap(),
            ["lookup:custom.return_values", "return:custom.return_values"]
        );
    }
}

#[test]
fn controlled_result_inference_rejects_foreign_callback_owners() {
    let budget = MemoryBudget::new(4096);
    let foreign_budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let foreign = ProductionControl::new(&foreign_budget, &token, &token);
    let mut infer = |_: &ScalarExpr| copy(Some(&ColumnType::Text), &foreign);
    let args = [ScalarExpr::Literal(Value::Null)];
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        builtin_function_type_with_control(
            FunctionTypeCall {
                name: "min",
                binding: None,
                args: &args,
            },
            &[],
            &[],
            None,
            &mut infer,
            &control,
        )
    }));
    assert!(result.is_err());
    assert_eq!(budget.used(), 0);
    assert_eq!(foreign_budget.used(), 0);
}
