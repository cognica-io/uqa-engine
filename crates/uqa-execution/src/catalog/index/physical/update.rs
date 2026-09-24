//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain index entries only when every indexed input keeps its stored representation.

use super::{column_value, PhysicalIndexDefinitions};
use uqa_sql::{
    ast::{Expr, IndexKey},
    catalog::index::EnforcedKey,
};
use uqa_storage::document_store::Document;

impl PhysicalIndexDefinitions {
    pub(crate) fn inputs_unchanged(
        &self,
        table: &str,
        enforced: &[EnforcedKey],
        previous: &Document,
        document: &Document,
    ) -> bool {
        let changed = |column: &str| {
            !column_value(previous, column).has_same_representation(column_value(document, column))
        };
        let row_changed = || {
            previous
                .keys()
                .chain(document.keys())
                .any(|column| changed(column))
        };
        let expression_changed = |expression: &Expr| {
            expression.any_node(&|node| match node {
                Expr::Column(column) | Expr::QualifiedColumn { column, .. } => {
                    if !previous.contains_key(column) && !document.contains_key(column) {
                        row_changed()
                    } else {
                        changed(column)
                    }
                }
                Expr::Star | Expr::QualifiedStar(_) | Expr::InternalColumn(_) => row_changed(),
                _ => false,
            })
        };
        let keys_changed = |keys: &[IndexKey], predicate: Option<&Expr>| {
            keys.iter().any(|key| match key {
                IndexKey::Column(column) => changed(column),
                IndexKey::Expression(expression) => expression_changed(expression),
            }) || predicate.is_some_and(expression_changed)
        };
        !enforced
            .iter()
            .any(|key| keys_changed(&key.keys, key.predicate.as_deref()))
            && !self
                .indexes
                .values()
                .filter(|index| index.table == table)
                .any(|index| {
                    keys_changed(&index.keys, index.definition.predicate.as_deref())
                        || index
                            .definition
                            .included_columns
                            .iter()
                            .any(|column| changed(column))
                })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::index::physical::PreparedIndex;
    use std::collections::BTreeMap;
    use uqa_core::Value;
    use uqa_sql::catalog::index::IndexDefinition;

    #[test]
    fn legacy_vector_index_retention_observes_expression_predicate_and_included_inputs() {
        let definitions = PhysicalIndexDefinitions {
            indexes: BTreeMap::from([(
                ("t".into(), "key".into()),
                PreparedIndex {
                    table: "t".into(),
                    method: "btree".into(),
                    keys: vec![IndexKey::Expression(Box::new(Expr::Column(
                        "expression_input".into(),
                    )))],
                    definition: IndexDefinition {
                        included_columns: vec!["included".into()],
                        predicate: Some(Box::new(Expr::Column("predicate_input".into()))),
                        ..IndexDefinition::default()
                    },
                },
            )]),
        };
        let previous = Document::from([
            ("expression_input".into(), Value::Float(0.0)),
            ("predicate_input".into(), Value::Bool(true)),
            ("included".into(), Value::Int(1)),
            ("unindexed".into(), Value::Int(1)),
        ]);
        assert!(definitions.inputs_unchanged("t", &[], &previous, &previous));
        for (column, value) in [
            ("expression_input", Value::Float(-0.0)),
            ("predicate_input", Value::Bool(false)),
            ("included", Value::Int(2)),
            ("unindexed", Value::Int(2)),
        ] {
            let mut current = previous.clone();
            current.insert(column.into(), value);
            assert_eq!(
                definitions.inputs_unchanged("t", &[], &previous, &current),
                column == "unindexed"
            );
            assert!(definitions.inputs_unchanged("other", &[], &previous, &current));
        }
    }
}
