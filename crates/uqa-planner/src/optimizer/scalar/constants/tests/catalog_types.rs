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
        if name == "text" {
            Some(ColumnType::Domain {
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
                    types
                )
                .unwrap(),
                cast,
            );
            assert!(!is_constant(&cast, types));
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
        assert!(is_constant(&cast, types));
        let folded = fold_authorized_literal(
            cast,
            uqa_execution::scalar::eval_constant_scalar,
            None,
            types,
        )
        .unwrap();
        assert!(
            matches!(folded, ScalarExpr::TypedLiteral { value: Value::Str(text), bound_type: Some(ColumnType::Text), .. } if text == "x")
        );
    }
}
