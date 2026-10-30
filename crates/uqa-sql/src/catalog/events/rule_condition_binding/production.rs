//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live rule-condition identities and row metadata retain their admitted destinations.

use super::RuleConditionBinding;
use crate::{
    ast::{ColumnDef, InternalRelationId},
    plan::{copy::SyntaxProduction, MutableExecutablePlan, MutableExpressionPlan, OwnedScalarExpr},
    RowSchema, SQLError, ScalarExpr,
};
use uqa_core::memory::{Produced, ProductionControl};

impl RuleConditionBinding {
    pub fn refresh_columns_with_control(
        &self,
        columns: &[ColumnDef],
        plan: MutableExpressionPlan,
        control: &ProductionControl<'_>,
    ) -> Result<(Produced<Self>, MutableExpressionPlan), SQLError> {
        let changed = !self
            .columns
            .iter()
            .map(String::as_str)
            .eq(columns.iter().map(|column| column.name.as_str()));
        let output = SyntaxProduction::new(*control);
        let mut names = output.vector(columns.len())?;
        for column in columns {
            names.push(output.text(&column.name)?)?;
        }
        let rebound = Self {
            old_relation: self.old_relation.map(|relation| {
                if changed {
                    InternalRelationId::allocate()
                } else {
                    relation
                }
            }),
            new_relation: self.new_relation.map(|relation| {
                if changed {
                    InternalRelationId::allocate()
                } else {
                    relation
                }
            }),
            columns: names.finish()?,
        };
        let rebound = output.finish(rebound)?;
        let plan = if changed {
            let plan = MutableExecutablePlan::Query(plan.into_projection_query(control)?);
            let plan = plan.transform_scalar_nodes(control, &mut |_, expression| {
                let ScalarExpr::InternalColumn(column) = &*expression else {
                    return Ok(expression);
                };
                let relation = if Some(column.relation()) == self.old_relation {
                    rebound.old_relation
                } else if Some(column.relation()) == self.new_relation {
                    rebound.new_relation
                } else {
                    None
                };
                match self
                    .column_name(*column)
                    .and_then(|name| rebound.column(relation, name))
                {
                    Some(column) => {
                        Ok(ScalarExpr::InternalColumn(column).copy_for_rewrite(control)?)
                    }
                    None => Ok(expression),
                }
            })?;
            let MutableExecutablePlan::Query(query) = plan else {
                unreachable!()
            };
            MutableExpressionPlan::from_projection_query(query, control)?
        } else {
            plan
        };
        Ok((rebound, plan))
    }

    pub fn whole_row_expression_with_control(
        &self,
        qualifier: &str,
        table: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<OwnedScalarExpr>, SQLError> {
        control.check()?;
        let Some(relation) = (match qualifier {
            "old" => self.old_relation,
            "new" => self.new_relation,
            _ => None,
        }) else {
            return Ok(None);
        };
        let mut items = crate::production::Buffer::new(control);
        for position in 0..self.columns.len() {
            items.push(
                ScalarExpr::InternalColumn(relation.column(position)).copy_for_rewrite(control)?,
                control,
            )?;
        }
        Ok(Some(OwnedScalarExpr::cast(
            OwnedScalarExpr::row(items.into_iter(), control)?,
            control.copy_text(table)?,
            false,
            control,
        )?))
    }

    pub fn row_schema_with_control(
        &self,
        columns: &[ColumnDef],
        control: &ProductionControl<'_>,
    ) -> Result<RowSchema, SQLError> {
        let mut schema = RowSchema::empty_with_control(control)?;
        for (side, relation) in [("old", self.old_relation), ("new", self.new_relation)] {
            let Some(relation) = relation else {
                continue;
            };
            let row = RowSchema::with_column_metadata_with_control(
                Some(side),
                columns
                    .iter()
                    .map(|column| (column.name.as_str(), Some(&column.ty), column.collation_oid)),
                control,
            )?;
            let row = RowSchema::with_physical_internal_aliases_with_control(
                &row,
                columns
                    .iter()
                    .enumerate()
                    .map(|(index, column)| (relation.column(index), index, Some(&column.ty))),
                control,
            )?;
            schema = RowSchema::join_with_control(&schema, &row, std::iter::empty(), control)?;
        }
        Ok(schema)
    }
}
