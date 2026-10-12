//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! RETURNING metadata comes from declarations, independently of row-image values.

use super::*;
use crate::ast::ColumnType;
use crate::semantics::{mutation_clause_schema, returning_context_schema, DOC_ID_COLUMN};

struct DeclaredTarget(Vec<ColumnDef>);

impl ReturningCatalog for DeclaredTarget {
    fn try_describe_table_row_type(&self, _: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        Ok(Some(self.0.clone()))
    }

    fn try_table_columns(&self, _: &str) -> Result<Vec<String>, String> {
        panic!("declared metadata must not require stored documents")
    }

    fn view_schema(&self, _: &str) -> Result<Option<RowSchema>, SQLError> {
        panic!("declared table is not a view")
    }
}

fn catalog() -> DeclaredTarget {
    let Statement::CreateTable(mut table) =
        crate::compile("CREATE TABLE t (c text, p text, n integer)")
            .unwrap()
            .remove(0)
    else {
        panic!("table declaration")
    };
    table.columns[0].collation_oid = Some(950);
    table.columns[1].collation_oid = Some(951);
    DeclaredTarget(table.columns)
}

#[test]
fn returning_collations_follow_declared_images_and_keep_system_columns_addressable() {
    let catalog = catalog();
    let target = returning_target_schema(&catalog, "t").unwrap();
    let aliases = ReturningAliases {
        old: "prior".into(),
        new: "next".into(),
        old_explicit: true,
        new_explicit: true,
    };
    let schema = returning_expression_schema(&target, "q", &aliases, None);
    let width = schema.len();
    assert_eq!(schema.physical_width(), width * 3);
    for (position, oid) in [950, 951, 0].into_iter().enumerate() {
        let column = &target.columns()[position];
        assert_eq!(target.column_collation(position), Some(oid));
        for (image, offset) in [("q", 0), ("prior", width), ("next", width * 2)] {
            assert_eq!(schema.qualified_collation(image, column), Some(oid));
            assert_eq!(
                schema.qualified_slot(image, column),
                Some(offset + position)
            );
        }
    }
    assert_eq!(schema.qualified_collation("old", "c"), None);
    assert_eq!(
        schema.qualified_type("prior", DOC_ID_COLUMN),
        Some(&ColumnType::BigInteger)
    );
    for position in target.len()..width {
        assert!(!schema.wildcard_position_visible(position));
        for image in ["q", "prior", "next"] {
            assert_eq!(
                schema.qualified_collation(image, &schema.columns()[position]),
                Some(0)
            );
        }
    }
    let clauses = mutation_clause_schema(&target, "q", None);
    assert_eq!(clauses.qualified_collation("q", "p"), Some(951));
    assert_eq!(clauses.qualified_collation("old", "c"), None);
    let expanded = expanded_returning_projections(
        &catalog,
        "t",
        "q",
        &aliases,
        &[ProjectionPlan {
            expr: crate::ScalarExpr::QualifiedStar("next".into()),
            alias: None,
        }],
    )
    .unwrap();
    assert_eq!(expanded.len(), target.len());
    assert_eq!(
        expanded
            .iter()
            .map(|p| p.alias.as_deref().unwrap())
            .collect::<Vec<_>>(),
        ["c", "p", "n"]
    );
}

#[test]
fn default_returning_images_need_no_selected_collation_metadata() {
    let schema = returning_context_schema(
        &["text".into(), "number".into()],
        &[Some(ColumnType::Text), Some(ColumnType::Integer)],
        2,
        "t",
        &ReturningAliases::default(),
    );
    assert_eq!(schema.collation_overrides().count(), 0);
    for image in ["t", "old", "new"] {
        assert_eq!(schema.qualified_collation(image, "text"), Some(100));
        assert_eq!(schema.qualified_collation(image, "number"), Some(0));
    }
}
