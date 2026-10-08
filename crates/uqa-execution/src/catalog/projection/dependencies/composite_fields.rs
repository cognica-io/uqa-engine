//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Adapt catalog expression scopes to SQL's named composite attribute dependency analysis.

use super::{ColumnScope, ExpressionReferences, References};
use uqa_sql::ast::Expr;
use uqa_sql::{RowSchema, SQLError};

pub(super) fn transition_schema(columns: &[uqa_sql::ast::ColumnDef]) -> RowSchema {
    let schema = RowSchema::with_types(
        columns.iter().map(|column| column.name.clone()).collect(),
        columns
            .iter()
            .map(|column| Some(column.ty.clone()))
            .collect(),
    );
    RowSchema::join(
        &RowSchema::with_relation_qualifier(&schema, "old"),
        &RowSchema::with_relation_qualifier(&schema, "new"),
        [],
    )
}

impl ExpressionReferences<'_> {
    pub(super) fn collect_composite_fields(
        &self,
        expression: &Expr,
        scope: ColumnScope<'_>,
        references: &mut References,
    ) -> Result<(), SQLError> {
        let plan = uqa_sql::plan::ExpressionPlan::lower(expression.clone());
        let schema = match scope {
            ColumnScope::None => RowSchema::new(Vec::new()),
            ColumnScope::Domain(base) => {
                RowSchema::with_types(vec!["value".into()], vec![Some(base.clone())])
            }
            ColumnScope::Relation(_, relation) | ColumnScope::Trigger(_, relation) => {
                let schema = RowSchema::with_types(
                    relation
                        .columns
                        .iter()
                        .map(|column| column.name.clone())
                        .collect(),
                    relation
                        .columns
                        .iter()
                        .map(|column| Some(column.ty.clone()))
                        .collect(),
                );
                if matches!(scope, ColumnScope::Trigger(..)) {
                    transition_schema(&relation.columns)
                } else {
                    RowSchema::with_relation_qualifier(&schema, &relation.identity.name)
                }
            }
        };
        let binding = uqa_sql::binding::BindingContext {
            catalog: std::sync::Arc::new(self.catalog.clone()),
            resolution: self.resolution.clone(),
            ctes: std::collections::BTreeMap::new(),
            deferred_ctes: std::collections::BTreeMap::new(),
            non_returning_ctes: std::collections::BTreeSet::new(),
            scalar_subqueries: &[],
        };
        for address in
            uqa_sql::binding::composite_dependencies::expression_plan_composite_dependencies(
                self.context.routines,
                &plan,
                &[],
                &binding,
                &schema,
            )?
        {
            references.add(address);
        }
        Ok(())
    }
}
