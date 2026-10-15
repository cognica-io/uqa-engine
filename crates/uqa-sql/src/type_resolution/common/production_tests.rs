//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn collated_unknown_inputs_charge_name_selection_to_common_type_allowance() {
    let crate::Statement::Select(query) = crate::compile("SELECT 'x' COLLATE \"C\"")
        .unwrap()
        .remove(0)
    else {
        panic!("query")
    };
    let expression = crate::plan::ExpressionPlan::lower(query.projections[0].expr.clone()).scalar;
    let schema = crate::RowSchema::default();
    let token = CancellationToken::new();
    let empty = MemoryBudget::new(0);
    let error = common_context_expression_type_with_control(
        &expression,
        &schema,
        &[],
        None,
        &ProductionControl::new(&empty, &token, &token),
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(empty.used(), 0);
    let budget = MemoryBudget::new(1024);
    assert!(common_context_expression_type_with_control(
        &expression,
        &schema,
        &[],
        None,
        &ProductionControl::new(&budget, &token, &token),
    )
    .unwrap()
    .is_none());
    assert!(budget.peak() > 0);
    assert_eq!(budget.used(), 0);
}

#[test]
fn common_type_control_preserves_domain_identity_and_mixed_array_rules() {
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let domain = ColumnType::Domain {
        collation_oid: None,
        schema: "schema".into(),
        name: "number".into(),
        oid: 40000,
        array_oid: None,
        base: Box::new(ColumnType::Integer),
    };
    let same = common_type_with_control(&domain, &domain, &control).unwrap();
    assert_eq!(*same, domain);
    assert!(same.reserved_bytes() > 0);
    drop(same);
    assert_eq!(budget.used(), 0);
    let base = common_type_with_control(&domain, &ColumnType::BigInteger, &control).unwrap();
    assert_eq!(*base, ColumnType::BigInteger);
    assert_eq!(base.reserved_bytes(), 0);
    let mixed = common_type_with_control(
        &ColumnType::Array(Box::new(ColumnType::Integer)),
        &ColumnType::Array(Box::new(ColumnType::BigInteger)),
        &control,
    )
    .unwrap();
    assert_eq!(*mixed, ColumnType::Array(Box::new(ColumnType::BigInteger)));
    assert_eq!(mixed.reserved_bytes(), size_of::<ColumnType>());
    drop(mixed);
    assert_eq!(budget.used(), 0);
}

#[test]
fn literal_type_inference_retains_only_its_selected_type() {
    let value = Value::List(vec![
        Value::List(vec![Value::Int(1), Value::Null]),
        Value::List(vec![Value::Int(i64::MAX)]),
    ]);
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let inferred = value_type_with_control(&value, &control).unwrap().unwrap();
    assert_eq!(
        *inferred,
        ColumnType::Array(Box::new(ColumnType::Array(Box::new(
            ColumnType::BigInteger
        ))))
    );
    assert_eq!(budget.used(), inferred.reserved_bytes());
    assert_eq!(inferred.reserved_bytes(), 2 * size_of::<ColumnType>());
    drop(inferred);
    assert_eq!(budget.used(), 0);
    assert!(value_type_with_control(
        &Value::List(vec![Value::Int(1), Value::Bool(false)]),
        &control
    )
    .unwrap()
    .is_none());
    assert_eq!(budget.used(), 0);
}

#[test]
fn literal_type_resource_errors_are_not_unknown_type_fallbacks() {
    let budget = MemoryBudget::new(size_of::<ColumnType>());
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let value = Value::List(vec![Value::List(vec![Value::Int(1)])]);
    let error = value_type_with_control(&value, &control).unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert!(budget.peak() > 0);
    assert_eq!(budget.used(), 0);
    for cancel_original in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        assert_eq!(
            value_type_with_control(&Value::Null, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn common_input_type_keeps_a_domain_only_when_every_input_has_it() {
    let control = ProductionControl::uncontrolled();
    let domain = ColumnType::Domain {
        collation_oid: None,
        schema: "public".into(),
        name: "positive".into(),
        oid: 40001,
        array_oid: None,
        base: Box::new(ColumnType::Integer),
    };
    let select = |types: &[Option<&ColumnType>]| {
        select_common_input_type_with_control(types, &control)
            .unwrap()
            .map(|ty| ty.into_uncontrolled().unwrap())
    };
    assert_eq!(
        select(&[Some(&domain), Some(&domain)]),
        Some(domain.clone())
    );
    // An `unknown` input differs from the domain, so the domain is reduced to its base type.
    assert_eq!(select(&[Some(&domain), None]), Some(ColumnType::Integer));
    assert_eq!(
        select(&[
            Some(&domain),
            Some(&ColumnType::Numeric {
                precision: None,
                scale: None
            })
        ]),
        Some(ColumnType::Numeric {
            precision: None,
            scale: None
        })
    );
    assert_eq!(select(&[None, None]), Some(ColumnType::Text));
    assert_eq!(
        select(&[Some(&ColumnType::Integer), Some(&ColumnType::Boolean)]),
        None
    );
    let known = ScalarExpr::Param(1);
    let unknown = ScalarExpr::Literal(Value::Null);
    let params = [SQLParam::typed_scalar(Value::Null, domain.clone())];
    for (items, expected) in [
        (vec![known.clone(), known.clone()], domain),
        (vec![known.clone(), unknown.clone()], ColumnType::Integer),
        (vec![unknown, known], ColumnType::Integer),
    ] {
        let rows = items.into_iter().map(|item| vec![item]).collect::<Vec<_>>();
        assert_eq!(
            values_column_types(&rows, &params).unwrap(),
            vec![Some(expected)]
        );
    }
}

#[test]
fn common_input_selection_retains_one_type_and_propagates_resource_errors() {
    let domain = ColumnType::Domain {
        collation_oid: Some(951),
        schema: "public".into(),
        name: "nonempty".repeat(128),
        oid: 50_010,
        array_oid: Some(50_011),
        base: Box::new(ColumnType::Text),
    };
    let token = CancellationToken::new();
    // Thousands of inputs borrow one catalog type; the allowance fits only its selected copy.
    let budget = MemoryBudget::new(2048);
    let control = ProductionControl::new(&budget, &token, &token);
    for count in [1, 4096] {
        let selected = select_common_input_type_in_with_control(
            CommonTypeContext::Coalesce,
            std::iter::repeat_n(Some(&domain), count),
            &control,
        )
        .unwrap();
        assert_eq!(*selected, domain);
        assert_eq!(budget.used(), selected.reserved_bytes());
        drop(selected);
        assert_eq!(budget.used(), 0);
        let selected = select_common_input_type_in_with_control(
            CommonTypeContext::Coalesce,
            std::iter::repeat_n(Some(&domain), count).chain([None]),
            &control,
        )
        .unwrap();
        assert_eq!(*selected, ColumnType::Text);
        assert_eq!(budget.used(), 0);
    }
    let empty = MemoryBudget::new(0);
    let control = ProductionControl::new(&empty, &token, &token);
    for context in [false, true] {
        let result = if context {
            select_common_input_type_in_with_control(
                CommonTypeContext::Array,
                [Some(&domain)].into_iter(),
                &control,
            )
            .map(Some)
        } else {
            select_common_input_type_with_control(&[Some(&domain)], &control)
        };
        assert_eq!(result.unwrap_err().sqlstate(), Some("53200"));
        assert_eq!(empty.used(), 0);
    }
    for inputs in [[Some(&domain), None], [None, Some(&domain)]] {
        assert_eq!(
            *select_common_input_type_with_control(&inputs, &control)
                .unwrap()
                .unwrap(),
            ColumnType::Text,
        );
        assert_eq!(
            *select_common_input_type_in_with_control(
                CommonTypeContext::Coalesce,
                inputs.into_iter(),
                &control,
            )
            .unwrap(),
            ColumnType::Text,
        );
        assert_eq!(empty.used(), 0);
    }
    token.cancel();
    assert_eq!(
        select_common_input_type_with_control(&[None, None], &control)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(
        select_common_input_type_in_with_control(
            CommonTypeContext::Array,
            [None, None].into_iter(),
            &control
        )
        .unwrap_err()
        .sqlstate(),
        Some("57014")
    );
    assert_eq!(empty.used(), 0);
}

#[test]
fn common_unknown_array_inputs_reuse_one_selected_type() {
    let ty = ColumnType::Array(Box::new(ColumnType::Varchar(None)));
    let budget = MemoryBudget::new(size_of::<ColumnType>());
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let selected = select_common_input_type_in_with_control(
        CommonTypeContext::Case,
        [Some(&ty)]
            .into_iter()
            .chain(std::iter::repeat_n(None, 4096)),
        &control,
    )
    .unwrap();
    assert_eq!(*selected, ty);
    assert_eq!(budget.used(), selected.reserved_bytes());
    assert_eq!(budget.peak(), size_of::<ColumnType>());
    drop(selected);
    assert_eq!(budget.used(), 0);
}
