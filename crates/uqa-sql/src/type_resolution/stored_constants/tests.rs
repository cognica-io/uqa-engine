//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected SQL operand types remain distinct from runtime carrier annotations.

use super::*;
use crate::{plan::ExpressionPlan, RowSchema};

struct Catalog;

impl FunctionTypeResolver for Catalog {
    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, SQLError> {
        let name = name.trim_matches('"');
        let (element, oid) = match name {
            "ints" => (ColumnType::Integer, 50_010),
            "bigints" => (ColumnType::BigInteger, 50_011),
            _ => return Ok(None),
        };
        Ok(Some(ColumnType::Domain {
            schema: "public".into(),
            name: name.into(),
            oid,
            array_oid: None,
            base: Box::new(ColumnType::Array(Box::new(element))),
        }))
    }
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&crate::ast::FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
}

fn expression(sql: &str) -> ScalarExpr {
    let crate::Statement::Select(mut query) =
        crate::compile(&format!("SELECT {sql}")).unwrap().remove(0)
    else {
        panic!("SELECT expression")
    };
    ExpressionPlan::lower(query.projections.remove(0).expr).scalar
}

fn schema() -> RowSchema {
    let numeric = ColumnType::Numeric {
        precision: Some(10),
        scale: Some(2),
    };
    RowSchema::with_types(
        ["n", "i", "b", "f", "d", "domain_value"]
            .map(str::to_string)
            .to_vec(),
        vec![
            Some(numeric.clone()),
            Some(ColumnType::Integer),
            Some(ColumnType::BigInteger),
            Some(ColumnType::Real),
            Some(ColumnType::DoublePrecision),
            Some(ColumnType::Domain {
                schema: "public".into(),
                name: "operand_numeric".into(),
                oid: 50_001,
                array_oid: None,
                base: Box::new(numeric),
            }),
        ],
    )
}

#[test]
fn stored_operators_keep_selected_inputs_without_result_type_or_carrier_casts() {
    for (input, expected) in [
        ("n + i", "n + i::numeric"),
        ("f + i", "f + i::double precision"),
        ("f + d", "f + d"),
        ("f + f", "f + f"),
        ("i + b", "i + b"),
        ("n < 100", "n < 100::numeric"),
        ("domain_value + n", "domain_value::numeric + n"),
        ("-n", "-n"),
        ("dynamic_column + f", "dynamic_column + f"),
    ] {
        let mut stored = expression(input);
        store_operand_coercions(&mut stored, &schema(), &[], &Catalog).unwrap();
        let mut expected = expression(expected);
        crate::plan::rewrite_scalar_expression(&mut expected, &mut |node| {
            if let ScalarExpr::Cast { implicit, .. } = node {
                *implicit = true;
            }
        });
        assert_eq!(stored, expected, "{input}");
        let once = stored.clone();
        assert!(!store_operand_coercions(&mut stored, &schema(), &[], &Catalog).unwrap());
        assert_eq!(stored, once, "repeated binding of {input}");
    }
}

#[test]
fn stored_operators_preserve_explicit_cast_origin() {
    for sql in ["n + i::numeric", "f::double precision + d", "i::bigint + b"] {
        let original = expression(sql);
        let mut stored = original.clone();
        assert!(!store_operand_coercions(&mut stored, &schema(), &[], &Catalog).unwrap());
        assert_eq!(stored, original, "{sql}");
    }
}

#[test]
fn array_domains_keep_only_required_base_conversions_and_bind_idempotently() {
    for (sql, expected) in [
        ("ARRAY[i]::ints", "ARRAY[i]::ints"),
        ("ARRAY[i]::bigints", "(ARRAY[i]::bigint[])::bigints"),
        ("ARRAY[]::ints", "(ARRAY[]::integer[])::ints"),
    ] {
        let mut stored = expression(sql);
        store_operand_coercions(&mut stored, &schema(), &[], &Catalog).unwrap();
        assert_eq!(stored, expression(expected), "{sql}");
        let once = stored.clone();
        assert!(!store_operand_coercions(&mut stored, &schema(), &[], &Catalog).unwrap());
        assert_eq!(stored, once);
    }
}

#[test]
fn stored_explicit_temporal_inputs_keep_creation_values_and_written_modifiers() {
    use crate::expr::DateOrderScope;
    use uqa_core::TemporalDateOrder;

    for (sql, expected) in [
        ("DATE '02/03/2020'", "2020-03-02"),
        (
            "'02/03/2020 10:20:30.123456'::timestamp(3)",
            "2020-03-02 10:20:30.123456",
        ),
    ] {
        let mut stored = expression(sql);
        let written_type = match &stored {
            ScalarExpr::Cast { ty, .. } => ty.clone(),
            _ => panic!("explicit cast"),
        };
        {
            let _scope = DateOrderScope::enter(TemporalDateOrder::DayMonthYear);
            assert!(store_operand_coercions(&mut stored, &schema(), &[], &Catalog).unwrap());
        }
        let ScalarExpr::Cast { expr, ty, implicit } = &stored else {
            panic!("written cast remains")
        };
        assert!(!implicit);
        assert_eq!(*ty, written_type);
        let ScalarExpr::TypedLiteral {
            value: Value::Temporal(value),
            ..
        } = expr.as_ref()
        else {
            panic!("input constant")
        };
        assert_eq!(value.to_sql_string(), expected);
        let once = stored.clone();
        let _scope = DateOrderScope::enter(TemporalDateOrder::YearMonthDay);
        assert!(!store_operand_coercions(&mut stored, &schema(), &[], &Catalog).unwrap());
        assert_eq!(stored, once);
    }
}
