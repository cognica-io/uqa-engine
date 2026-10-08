//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rename stored field selectors by their scoped composite identity, without evaluating inputs.

use super::{BindingContext, SchemaScope};
use crate::{
    ast::{CreateFunction, Expr, FunctionBinding, Statement},
    plan::{ExpressionPlan, QueryPlan, UnifiedPlan},
    routines::RoutineResolution,
    RowSchema, SQLError, SQLParam, ScalarExpr,
};
use uqa_core::Value;

pub struct CompositeFieldRename<'a> {
    pub target: u32,
    pub number: i16,
    pub to: &'a str,
    pub routines: &'a dyn RoutineResolution,
    pub binding: BindingContext<'a>,
}

#[cfg(test)]
mod tests;

impl CompositeFieldRename<'_> {
    pub fn relation_schema(&self, name: &str) -> Result<RowSchema, SQLError> {
        let (_, qualifier) =
            uqa_core::RelationIdentity::parse_reference(name).map_err(SQLError::Internal)?;
        super::sources::analyze_source_plan_schema(
            self.routines,
            &crate::plan::SourcePlan::Table {
                name: name.to_owned(),
                qualifier,
                alias: None,
                column_aliases: Vec::new(),
                include_descendants: true,
                bound_columns: None,
            },
            &[],
            &self.binding,
            None,
        )
    }

    pub fn transition_schema(&self, name: &str) -> Result<RowSchema, SQLError> {
        let schema = self.relation_schema(name)?;
        Ok(RowSchema::join(
            &RowSchema::with_relation_qualifier(&schema, "old"),
            &RowSchema::with_relation_qualifier(&schema, "new"),
            [],
        ))
    }
    fn scope(&self) -> Result<SchemaScope, SQLError> {
        let mut scope = SchemaScope::for_analysis(&self.binding)?;
        scope.scalar_binding = super::ScalarBindingMode::CompositeInputs;
        scope.preserve_syntax_shape = true;
        Ok(scope)
    }

    fn selected(&self, binding: Option<&FunctionBinding>) -> bool {
        binding
            .and_then(|binding| binding.composite_field.as_ref())
            .is_some_and(|field| field.type_oid == self.target && field.number == self.number)
    }

    fn syntax_node(&self, node: &mut Expr) -> bool {
        if let Expr::Func { binding, args, .. } = node {
            if self.selected(binding.as_ref()) {
                if let Some(Expr::Literal(Value::Str(name))) = args.get_mut(1) {
                    if name != self.to {
                        self.to.clone_into(name);
                        return true;
                    }
                }
            }
        }
        false
    }

    fn scalar_node(&self, node: &mut ScalarExpr) -> bool {
        if let ScalarExpr::Func { binding, args, .. } = node {
            if self.selected(binding.as_ref()) {
                if let Some(ScalarExpr::Literal(Value::Str(name))) = args.get_mut(1) {
                    if name != self.to {
                        self.to.clone_into(name);
                        return true;
                    }
                }
            }
        }
        false
    }

    pub fn expression(&self, expression: &mut Expr, schema: &RowSchema) -> Result<bool, SQLError> {
        let lowered = ExpressionPlan::lower(expression.clone());
        let mut plan = lowered.clone();
        self.bind_expression_plan(&mut plan, schema)?;
        let sites = super::syntax_sites::expression_syntax_sites(&lowered, &plan)?;
        let mut next = expression.clone();
        crate::catalog::stored_ast::bind_stored_expression_sites(&mut next, &sites)?;
        let mut changed = false;
        crate::catalog::stored_ast::visit_stored_expression(&mut next, &mut |node| {
            changed |= self.syntax_node(node);
            Ok(())
        })?;
        if changed {
            *expression = next;
        }
        Ok(changed)
    }

    fn bind_expression_plan(
        &self,
        plan: &mut ExpressionPlan,
        schema: &RowSchema,
    ) -> Result<(), SQLError> {
        let mut scope = self.scope()?;
        scope.stored_expression_outer = Some(schema.clone());
        for query in &mut plan.subqueries {
            scope.bind_query_routines_for_storage(self.routines, query, &[], Some(schema))?;
        }
        scope.bind_scalar_routines_for_storage(
            self.routines,
            &mut plan.scalar,
            schema,
            &plan.subqueries,
            &[],
        )
    }

    pub fn expression_plan(
        &self,
        plan: &mut ExpressionPlan,
        schema: &RowSchema,
    ) -> Result<bool, SQLError> {
        let mut next = plan.clone();
        self.bind_expression_plan(&mut next, schema)?;
        let mut changed = false;
        crate::plan::rewrite_scalar_expression(&mut next.scalar, &mut |node| {
            changed |= self.scalar_node(node);
        });
        for query in &mut next.subqueries {
            query.rewrite_scalar_expressions(&mut |node| changed |= self.scalar_node(node));
        }
        if changed {
            *plan = next;
        }
        Ok(changed)
    }

    pub fn query(&self, query: &mut QueryPlan) -> Result<bool, SQLError> {
        let mut next = query.clone();
        self.scope()?
            .bind_query_routines_for_storage(self.routines, &mut next, &[], None)?;
        let mut changed = false;
        next.rewrite_scalar_expressions(&mut |node| changed |= self.scalar_node(node));
        if changed {
            *query = next;
        }
        Ok(changed)
    }

    pub fn statement(
        &self,
        statement: &mut Statement,
        definition: Option<&CreateFunction>,
        outer: Option<&RowSchema>,
    ) -> Result<bool, SQLError> {
        let mut plan = UnifiedPlan::lower_with(statement.clone(), &|name: &str| {
            self.routines.has_registered_aggregate_function(name)
        });
        super::stored_routines::mark_catalog_statement_relations_bound(&mut plan)?;
        let params = if let Some(definition) = definition {
            let params = self.routine_params(definition)?;
            let scope =
                crate::routines::body_parameters::sql_body_parameter_scope(definition, &params)?;
            super::bind_routine_parameter_references(
                self.routines,
                &mut plan,
                &params,
                &self.binding,
                &scope,
            )?;
            params
        } else {
            Vec::new()
        };
        let bound = super::stored_routines::bind_catalog_statement_composite_inputs(
            &super::stored_routines::CatalogRoutineContext {
                routines: self.routines,
                binding: &self.binding,
            },
            &plan,
            &params,
            outer,
        )?;
        let mut next = statement.clone();
        crate::catalog::stored_ast::bind_stored_statement_sites(&mut next, &bound.sites)?;
        let mut changed = false;
        crate::catalog::stored_ast::visit_stored_statement_expressions(&mut next, &mut |node| {
            changed |= self.syntax_node(node);
            Ok(())
        })?;
        if changed {
            *statement = next;
        }
        Ok(changed)
    }

    pub fn bound_plan(&self, plan: &mut UnifiedPlan) -> bool {
        let mut changed = false;
        plan.rewrite_scalar_expressions(&mut |node| changed |= self.scalar_node(node));
        changed
    }

    pub fn bind_routine_plan(
        &self,
        plan: &mut UnifiedPlan,
        definition: &CreateFunction,
    ) -> Result<(), SQLError> {
        let params = self.routine_params(definition)?;
        let parameters =
            crate::routines::body_parameters::sql_body_parameter_scope(definition, &params)?;
        let mut scope = self.scope()?;
        scope.routine_parameters = Some(parameters.clone());
        scope.bind_statement_parameters(self.routines, plan, &params, Some(parameters.schema()))
    }

    fn routine_params(&self, definition: &CreateFunction) -> Result<Vec<SQLParam>, SQLError> {
        crate::routines::body_parameters::sql_body_parameters(definition)
            .into_iter()
            .map(|parameter| {
                self.routines
                    .resolve_type_name(&parameter.type_name)
                    .map(|ty| {
                        ty.map_or_else(
                            || SQLParam::scalar(Value::Null),
                            |ty| SQLParam::typed_scalar(Value::Null, ty),
                        )
                    })
            })
            .collect()
    }
}
