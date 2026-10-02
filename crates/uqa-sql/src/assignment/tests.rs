//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::conversion::coerce_json_value;
use uqa_core::Value;

#[test]
fn json_coercion_rejects_invalid_json_strings() {
    assert!(coerce_json_value(Value::Str("{invalid".into()), true).is_err());
    assert!(matches!(
        coerce_json_value(Value::Str("{\"ok\":true}".into()), true).unwrap(),
        Value::JsonB(_)
    ));
}

mod column_shapes {
    use crate::assignment::columns::{
        generated_column_kind, AssignmentColumnCatalog, ColumnCatalogError, ColumnShape,
    };
    use crate::ast::{ColumnDef, ColumnType, Expr, GeneratedColumnKind};
    use crate::{SQLError, Statement};

    /// A catalog that only describes whole tables.
    struct Described(Vec<ColumnDef>);

    impl AssignmentColumnCatalog for Described {
        fn try_describe_table(
            &self,
            table: &str,
        ) -> Result<Option<Vec<ColumnDef>>, ColumnCatalogError> {
            Ok((table == "public.t").then(|| self.0.clone()))
        }
        fn columns_declared(&self, _table: &str) -> Result<bool, ColumnCatalogError> {
            Ok(true)
        }
        fn try_column_insert_default_expr(
            &self,
            _table: &str,
            _column: &str,
        ) -> Result<Option<Expr>, ColumnCatalogError> {
            Ok(None)
        }
    }

    #[test]
    fn a_column_shape_is_the_type_and_generated_kind_a_description_holds() {
        let Statement::CreateTable(table) = crate::compile(
            "CREATE TABLE t (a integer, b text GENERATED ALWAYS AS (upper('x')) STORED, c bigint GENERATED ALWAYS AS (a + 1) VIRTUAL)",
        )
        .unwrap()
        .remove(0) else {
            panic!("expected a table definition");
        };
        let catalog = Described(table.columns);
        let shape = |column| catalog.try_column_shape("public.t", column).unwrap();
        assert_eq!(
            shape("a"),
            Some(Some(ColumnShape {
                ty: ColumnType::Integer,
                generated: None,
                identity_sequence: None
            }))
        );
        assert_eq!(
            shape("b"),
            Some(Some(ColumnShape {
                ty: ColumnType::Text,
                generated: Some(GeneratedColumnKind::Stored),
                identity_sequence: None
            }))
        );
        assert_eq!(
            shape("c").unwrap().unwrap().generated,
            Some(GeneratedColumnKind::Virtual)
        );
        // A table that declares no such column, and no such table.
        assert_eq!(shape("missing"), Some(None));
        assert_eq!(catalog.try_column_shape("public.other", "a").unwrap(), None);

        assert_eq!(
            generated_column_kind(&catalog, "public.t", "b").unwrap(),
            Some(GeneratedColumnKind::Stored)
        );
        assert_eq!(
            generated_column_kind(&catalog, "public.t", "a").unwrap(),
            None
        );
        assert_eq!(
            generated_column_kind(&catalog, "public.t", "missing").unwrap(),
            None
        );
        assert!(matches!(
            generated_column_kind(&catalog, "public.other", "a"),
            Err(SQLError::UnknownTable(table)) if table == "public.other"
        ));
    }
}
