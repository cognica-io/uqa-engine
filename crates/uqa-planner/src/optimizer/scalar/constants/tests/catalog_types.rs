//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Constant folding requires the type selected by the catalog, including for NULL input.

use super::*;
use uqa_sql::routines::declaration::RoutineTypeCatalog;

struct Catalog;

impl RoutineTypeCatalog for Catalog {
    fn try_describe_table(&self, _: &str) -> Result<Option<Vec<uqa_sql::ast::ColumnDef>>, String> {
        Ok(None)
    }

    fn resolve_catalog_column_type(&self, name: &str) -> Option<ColumnType> {
        if name == "enum#50020" {
            Some(ColumnType::Enum(uqa_sql::ast::EnumTypeReference {
                schema: "first".into(),
                name: "labels".into(),
                oid: 50_020,
                array_oid: 50_021,
            }))
        } else if name == "text" {
            Some(ColumnType::Domain {
                collation_oid: None,
                schema: "first".into(),
                name: "text".into(),
                oid: 50_010,
                array_oid: Some(50_011),
                base: Box::new(ColumnType::Text),
            })
        } else {
            ColumnType::from_sql_name(name).ok()
        }
    }

    fn resolve_catalog_column_type_name(&self, name: &str) -> Result<ColumnType, SQLError> {
        self.resolve_catalog_column_type(name)
            .ok_or_else(|| SQLError::Internal(format!("unknown test type {name}")))
    }

    fn resolve_catalog_user_type_by_oid(&self, _: u32) -> Option<ColumnType> {
        None
    }

    fn require_type_usage(&self, _: &ColumnType) -> Result<(), SQLError> {
        Ok(())
    }

    fn format_type(&self, ty: &ColumnType) -> Result<String, SQLError> {
        Ok(ty.sql_name())
    }
}

impl FunctionTypeResolver for Catalog {
    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, SQLError> {
        self.resolve_catalog_column_type_name(name).map(Some)
    }

    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&uqa_sql::ast::FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
}

#[test]
fn a_free_type_identifier_cannot_fold_before_catalog_selection() {
    for value in [Value::Null, Value::Str("x".into())] {
        let cast = ScalarExpr::Cast {
            implicit: false,
            expr: Box::new(ScalarExpr::Literal(value)),
            ty: "text".into(),
        };
        for types in [None, Some(&Catalog as &dyn RoutineTypeCatalog)] {
            assert_eq!(
                fold_authorized_literal(
                    cast.clone(),
                    |_| panic!("domain input cannot be folded"),
                    None,
                    types,
                    None,
                )
                .unwrap(),
                cast,
            );
            assert!(!is_constant(&cast, types, None));
        }
    }
}

#[test]
fn selected_builtin_identity_keeps_ordinary_constant_folding() {
    for types in [None, Some(&Catalog as &dyn RoutineTypeCatalog)] {
        let cast = ScalarExpr::Cast {
            implicit: false,
            expr: Box::new(ScalarExpr::Literal(Value::Str("x".into()))),
            ty: ColumnType::Text.catalog_name(),
        };
        assert!(is_constant(&cast, types, None));
        let folded = fold_authorized_literal(
            cast,
            uqa_execution::scalar::eval_constant_scalar,
            None,
            types,
            None,
        )
        .unwrap();
        assert!(
            matches!(folded, ScalarExpr::TypedLiteral { value: Value::Str(text), bound_type: Some(ColumnType::Text), .. } if text == "x")
        );
    }
}

#[test]
fn retained_enum_constants_use_the_same_catalog_during_inference_and_coercion() {
    let literal = |key| ScalarExpr::TypedLiteral {
        value: Value::Enum(uqa_core::EnumValue::new(
            50_020,
            uqa_core::EnumLabelKey::from_bytes(vec![key]).unwrap(),
        )),
        ty: "enum#50020".into(),
        bound_type: None,
        parameter_index: None,
        collation_oid: None,
        composite_source: None,
    };
    for key in [1, 2] {
        for op in [
            uqa_sql::ast::BinaryOp::Equal,
            uqa_sql::ast::BinaryOp::NotEqual,
        ] {
            let expression = ScalarExpr::Binary {
                op,
                lhs: Box::new(literal(1)),
                rhs: Box::new(literal(key)),
            };
            let expected = (key == 1) == (op == uqa_sql::ast::BinaryOp::Equal);
            let folded = fold_authorized_literal(
                expression,
                uqa_execution::scalar::eval_constant_scalar,
                None,
                Some(&Catalog),
                Some(&Catalog),
            )
            .unwrap();
            assert_eq!(folded, ScalarExpr::Literal(Value::Bool(expected)));
        }
    }
}
