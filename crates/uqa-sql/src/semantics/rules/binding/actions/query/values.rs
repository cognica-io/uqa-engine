//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain VALUES target-list substitution and per-row DEFAULT identity across view layers.

use super::projection_block;
use crate::{
    ast::{AssignmentTarget, ColumnDef, Expr, InternalRelationId},
    catalog::roles::RoleReference,
    plan::{ExpressionPlan, InsertPlan, ProjectionPlan, QueryPlan, RelationalPlan, SourcePlan},
    semantics::rules::analysis::{rule_returning_columns, RuleAnalysisContext},
    ColumnType, SQLError, ScalarExpr,
};
use uqa_core::Value;

pub(super) use crate::plan::RuleValuesInput as ValuesInput;

mod owned;
pub(super) use owned::default_expression as default_expression_with_control;

impl ValuesInput {
    pub(crate) fn describe(insert: &InsertPlan) -> Result<Option<Self>, SQLError> {
        if insert.source.is_some() {
            return Ok(insert.rule_values_input.clone());
        }
        let rows = &insert.rows;
        let inline = rows.len() == 1;
        let width = rows.first().map_or(insert.columns.len(), Vec::len);
        if rows.iter().any(|row| row.len() != width) {
            return Err(SQLError::Routine {
                sqlstate: "42601".into(),
                message: "VALUES lists must all be the same length".into(),
            });
        }
        let defaults = (0..width)
            .filter(|position| {
                rows.iter()
                    .any(|row| matches!(row[*position], ScalarExpr::Default))
            })
            .collect::<Vec<_>>();
        Ok(Some(Self {
            width,
            inline,
            unresolved: defaults.iter().copied().collect(),
            defaults,
            written: (0..width)
                .filter(|position| {
                    rows.iter()
                        .any(|row| !matches!(row[*position], ScalarExpr::Default))
                })
                .collect(),
        }))
    }

    pub(crate) fn source(
        &self,
        context: RuleAnalysisContext<'_>,
        insert: &mut InsertPlan,
    ) -> Result<Box<QueryPlan>, SQLError> {
        let columns = input_columns(context, insert, self.width)?;
        if let Some(source) = &insert.source {
            return Ok(source.clone());
        }
        let mut rows = insert.rows.clone();
        if self.inline {
            let mut block = projection_block(
                rows.pop()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|expr| ProjectionPlan { expr, alias: None })
                    .collect(),
            );
            block.subqueries.clone_from(&insert.subqueries);
            return Ok(Box::new(query(
                insert,
                RelationalPlan::QueryBlock(Box::new(block)),
            )));
        }
        for row in &mut rows {
            let defaults = self
                .defaults
                .iter()
                .map(|position| matches!(row[*position], ScalarExpr::Default))
                .collect::<Vec<_>>();
            for (value, (_, ty)) in row.iter_mut().zip(&columns) {
                *value = if matches!(value, ScalarExpr::Default) {
                    ScalarExpr::TypedLiteral {
                        value: Value::Null,
                        ty: ty.catalog_name(),
                        bound_type: Some(ty.clone()),
                        parameter_index: None,
                        collation_oid: None,
                        composite_source: None,
                    }
                } else {
                    cast(value.clone(), ty)
                };
            }
            row.extend(
                defaults
                    .into_iter()
                    .map(|value| ScalarExpr::Literal(Value::Bool(value))),
            );
        }
        Ok(Box::new(query(
            insert,
            RelationalPlan::Values {
                rows,
                subqueries: insert.subqueries.clone(),
            },
        )))
    }

    pub(crate) fn project_layer(
        &mut self,
        context: RuleAnalysisContext<'_>,
        insert: &mut InsertPlan,
    ) -> Result<(), SQLError> {
        let columns = input_columns(context, insert, self.width)?;
        let defaults = columns
            .iter()
            .map(|(column, _)| {
                if overrides_identity(column, insert.overriding) {
                    Ok(Some(Expr::Literal(Value::Null)))
                } else {
                    default_expression(context, column)
                }
            })
            .collect::<Result<Vec<_>, SQLError>>()?;
        insert.rows.clear();
        let source = insert.source.as_mut().expect("VALUES input query");
        if self.inline {
            let RelationalPlan::QueryBlock(block) = &mut source.root else {
                return Err(SQLError::Internal(
                    "rule target list lost its query block".into(),
                ));
            };
            for (position, (_, ty)) in columns.iter().enumerate() {
                let value = if self.unresolved.contains(&position) {
                    if defaults[position].is_some() {
                        self.unresolved.remove(&position);
                    }
                    default_value(defaults[position].as_ref(), &mut block.subqueries)
                } else {
                    block.projections[position].expr.clone()
                };
                block.projections[position].expr = cast(value, ty);
            }
            retain_authority(
                &mut block.subqueries,
                source.relation_privilege_subject.as_ref(),
            );
            source.analyzed_output = None;
            source.analyzed_source = None;
            insert.rule_values_input = Some(self.clone());
            return Ok(());
        }
        insert.rule_values_input = Some(self.clone());
        if self.defaults.is_empty() {
            return Ok(());
        }
        let relation = InternalRelationId::allocate();
        let mut block = projection_block(Vec::new());
        for (position, (_, ty)) in columns.iter().enumerate() {
            let value = ScalarExpr::InternalColumn(relation.column(position));
            let value =
                if let Some(flag) = self.defaults.iter().position(|value| *value == position) {
                    ScalarExpr::Case {
                        base: None,
                        when: vec![(
                            ScalarExpr::InternalColumn(relation.column(self.width + flag)),
                            cast(
                                default_value(defaults[position].as_ref(), &mut block.subqueries),
                                ty,
                            ),
                        )],
                        else_branch: Some(Box::new(value)),
                    }
                } else {
                    value
                };
            block.projections.push(ProjectionPlan {
                expr: value,
                alias: None,
            });
        }
        for (flag, position) in self.defaults.iter().enumerate() {
            let expr = if defaults[*position].is_some() {
                ScalarExpr::Literal(Value::Bool(false))
            } else {
                ScalarExpr::InternalColumn(relation.column(self.width + flag))
            };
            block.projections.push(ProjectionPlan { expr, alias: None });
        }
        let mut result = query(insert, RelationalPlan::QueryBlock(Box::new(block)));
        let source = insert.source.take().expect("VALUES input query");
        result
            .relation_privilege_subject
            .clone_from(&source.relation_privilege_subject);
        let RelationalPlan::QueryBlock(block) = &mut result.root else {
            unreachable!()
        };
        retain_authority(
            &mut block.subqueries,
            result.relation_privilege_subject.as_ref(),
        );
        block.from = Some(source_node(source, relation));
        insert.source = Some(Box::new(result));
        Ok(())
    }

    pub(crate) fn finish(&self, mut source: Box<QueryPlan>) -> Box<QueryPlan> {
        if self.inline || self.defaults.is_empty() {
            return source;
        }
        let relation = InternalRelationId::allocate();
        let mut block = projection_block(
            (0..self.width)
                .map(|position| ProjectionPlan {
                    expr: ScalarExpr::InternalColumn(relation.column(position)),
                    alias: None,
                })
                .collect(),
        );
        let subject = source.relation_privilege_subject.clone();
        let bound = source.relations_bound;
        source.analyzed_output = None;
        block.from = Some(source_node(source, relation));
        Box::new(QueryPlan {
            analyzed_output: None,
            analyzed_source: None,
            analyzed_grouping: None,
            relation_privilege_subject: subject,
            relations_bound: bound,
            ctes: Vec::new(),
            root: RelationalPlan::QueryBlock(Box::new(block)),
        })
    }
}

fn source_node(body: Box<QueryPlan>, relation: InternalRelationId) -> SourcePlan {
    SourcePlan::Subquery {
        captured_output: Some(relation),
        lateral: false,
        body,
        alias: None,
        column_aliases: Vec::new(),
    }
}

fn query(insert: &InsertPlan, root: RelationalPlan) -> QueryPlan {
    QueryPlan {
        analyzed_output: None,
        analyzed_source: None,
        analyzed_grouping: None,
        relation_privilege_subject: None,
        relations_bound: insert.relations_bound,
        ctes: Vec::new(),
        root,
    }
}

fn input_columns(
    context: RuleAnalysisContext<'_>,
    insert: &mut InsertPlan,
    width: usize,
) -> Result<Vec<(ColumnDef, ColumnType)>, SQLError> {
    let definitions = rule_returning_columns(context, &insert.table)?;
    if insert.columns.is_empty() {
        insert.columns = definitions
            .iter()
            .take(width)
            .map(|column| AssignmentTarget::from(column.name.clone()))
            .collect();
    }
    if insert.columns.len() != width {
        return Err(SQLError::Routine {
            sqlstate: "42601".into(),
            message: if insert.columns.len() < width {
                "INSERT has more expressions than target columns"
            } else {
                "INSERT has more target columns than expressions"
            }
            .into(),
        });
    }
    insert
        .columns
        .iter()
        .map(|target| {
            let column = definitions
                .iter()
                .find(|column| column.name == target.column)
                .ok_or_else(|| SQLError::UnknownColumn(target.column.clone()))?;
            let ty = crate::assignment::targets::field_assignment_types(
                target,
                &column.ty,
                context.composites,
            )?
            .pop()
            .expect("assignment input type");
            Ok((column.clone(), ty))
        })
        .collect()
}

pub(super) fn overrides_identity(
    column: &ColumnDef,
    overriding: Option<crate::ast::OverridingKind>,
) -> bool {
    overriding == Some(crate::ast::OverridingKind::UserValue)
        && column
            .auto_increment
            .as_ref()
            .is_some_and(crate::ast::AutoIncrement::is_identity)
}

pub(super) fn default_expression(
    context: RuleAnalysisContext<'_>,
    column: &ColumnDef,
) -> Result<Option<Expr>, SQLError> {
    if let Some(identity) = column
        .auto_increment
        .as_ref()
        .filter(|identity| identity.is_identity())
    {
        let sequence = identity
            .sequence
            .as_deref()
            .ok_or_else(|| SQLError::Routine {
                sqlstate: "XX000".into(),
                message: "no owned sequence found".into(),
            })?;
        return Ok(Some(
            crate::schema::sequences::implicit::sequence_next_value(sequence),
        ));
    }
    Ok(column
        .default
        .clone()
        .or_else(|| crate::catalog::domain::domain_default_expression(context.domains, &column.ty)))
}

fn default_value(default: Option<&Expr>, subqueries: &mut Vec<QueryPlan>) -> ScalarExpr {
    crate::plan::input_projection::append_expression(
        subqueries,
        ExpressionPlan::lower(default.cloned().unwrap_or(Expr::Literal(Value::Null))),
    )
}

fn cast(expr: ScalarExpr, ty: &ColumnType) -> ScalarExpr {
    ScalarExpr::Cast {
        implicit: true,
        expr: Box::new(expr),
        ty: ty.catalog_name(),
    }
}

pub(super) fn retain_authority(queries: &mut [QueryPlan], subject: Option<&RoleReference>) {
    if let Some(subject) = subject {
        for query in queries {
            query
                .relation_privilege_subject
                .get_or_insert(subject.clone());
        }
    }
}
