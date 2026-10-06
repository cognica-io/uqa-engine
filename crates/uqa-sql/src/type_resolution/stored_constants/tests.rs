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
