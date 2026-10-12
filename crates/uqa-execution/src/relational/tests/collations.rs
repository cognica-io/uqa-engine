//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projection preserves independent SQL collations while sharing original row payloads.

use super::*;
use crate::batch::{ColumnIdentity, PhysicalLayout};
use uqa_sql::ast::ColumnType;

#[test]
fn collation_projection_shares_payloads_and_preserves_each_bound_identity() {
    let schema = RowSchema::from_physical_layout(PhysicalLayout {
        columns: vec!["c".into(), "p".into()],
        identities: vec![
            ColumnIdentity::qualified("source", "c"),
            ColumnIdentity::qualified("source", "p"),
        ],
        types: vec![Some(ColumnType::Text); 2],
        slots: vec![Some(0); 2],
        physical_width: 1,
        aliases: vec![],
        internal: vec![],
        score_sources: vec![],
        wildcard_hidden: std::collections::HashSet::default(),
    })
    .unwrap()
    .with_column_collations([Some(950), Some(951)]);
    let schema = RowSchema::with_identity_aliases(
        &schema,
        &[
            (ColumnIdentity::qualified("aliases", "c"), 0),
            (ColumnIdentity::qualified("aliases", "p"), 1),
        ],
    );
    let original = crate::PhysicalRow::from_values(vec![Value::Str("x".repeat(4096))]);
    for expressions in [
        vec![("p", col("p")), ("c", col("c"))],
        vec![("ignored", ScalarExpr::Star)],
        vec![("ignored", ScalarExpr::QualifiedStar("source".into()))],
        vec![("ignored", ScalarExpr::QualifiedStar("aliases".into()))],
        vec![
            (
                "p",
                ScalarExpr::QualifiedColumn {
                    qualifier: "aliases".into(),
                    column: "p".into(),
                },
            ),
            (
                "c",
                ScalarExpr::QualifiedColumn {
                    qualifier: "aliases".into(),
                    column: "c".into(),
                },
            ),
        ],
    ] {
        let scan = TableScan::from_physical_rows(schema.clone(), vec![original.clone()]);
        let mut project = Project::new(
            Box::new(scan),
            expressions
                .into_iter()
                .map(|(name, expression)| (name.into(), expression))
                .collect(),
            vec![],
        );
        project.open().unwrap();
        let batch = project.next().unwrap().unwrap();
        project.close().unwrap();
        assert_eq!(batch.schema.collation_of("c"), Some(950));
        assert_eq!(batch.schema.collation_of("p"), Some(951));
        assert_eq!(batch.schema.physical_width(), 1);
        assert_eq!(batch.rows[0].fragment_count(), original.fragment_count());
        assert!(std::ptr::eq(
            batch.rows[0].value(0).unwrap(),
            original.value(0).unwrap()
        ));
    }
}
