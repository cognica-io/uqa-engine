//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable relation binding and dependency traversal for rewrite rules.

use std::collections::BTreeSet;

use uqa_planner::{QueryPlan, RelationalPlan, SourcePlan};
use uqa_sql::ast::{Expr, FrameBound, FromClause, SelectStmt, Statement};
use uqa_sql::SQLError;

use crate::engine_capabilities::{RelationLookupMode, RelationResolution};
use crate::{Engine, RelationIdentity};

use super::{RuleDependencies, RuleRoutineDependency, StoredRule};

struct RuleAstVisitor<'a, R, F> {
    visit_relation: &'a mut R,
    visit_routine: &'a mut F,
}

impl<R, F> RuleAstVisitor<'_, R, F>
where
    R: FnMut(&mut String) -> Result<(), SQLError>,
    F: FnMut(
        &mut String,
        Option<&mut Option<uqa_sql::ast::FunctionBinding>>,
    ) -> Result<(), SQLError>,
{
    fn bind_statement(&mut self, statement: &mut Statement) -> Result<(), SQLError> {
        let ctes = BTreeSet::new();
        match statement {
            Statement::Select(query) => self.bind_select(query, &ctes),
            Statement::Insert(insert) => {
                (self.visit_relation)(&mut insert.table)?;
                let visible = self.bind_ctes(&mut insert.with, &ctes)?;
                if let Some(source) = insert.select_source.as_deref_mut() {
                    self.bind_select(source, &visible)?;
                }
                for row in &mut insert.rows {
                    for expression in row {
                        self.bind_expr(expression, &visible)?;
                    }
                }
                if let Some(conflict) = &mut insert.on_conflict {
                    if let uqa_sql::ast::OnConflictAction::Update {
                        assignments,
                        r#where,
                    } = &mut conflict.action
                    {
                        for (_, expression) in assignments {
                            self.bind_expr(expression, &visible)?;
                        }
                        if let Some(expression) = r#where {
                            self.bind_expr(expression, &visible)?;
                        }
                    }
                }
                for projection in &mut insert.returning {
                    self.bind_expr(&mut projection.expr, &visible)?;
                }
                Ok(())
            }
            Statement::Update(update) => {
                (self.visit_relation)(&mut update.table)?;
                let visible = self.bind_ctes(&mut update.with, &ctes)?;
                if let Some(source) = &mut update.from {
                    self.bind_from(source, &visible)?;
                }
                for (_, expression) in &mut update.assignments {
                    self.bind_expr(expression, &visible)?;
                }
                if let Some(expression) = &mut update.r#where {
                    self.bind_expr(expression, &visible)?;
                }
                for projection in &mut update.returning {
                    self.bind_expr(&mut projection.expr, &visible)?;
                }
                Ok(())
            }
            Statement::Delete(delete) => {
                (self.visit_relation)(&mut delete.table)?;
                let visible = self.bind_ctes(&mut delete.with, &ctes)?;
                if let Some(source) = &mut delete.using {
                    self.bind_from(source, &visible)?;
                }
                if let Some(expression) = &mut delete.r#where {
                    self.bind_expr(expression, &visible)?;
                }
                for projection in &mut delete.returning {
                    self.bind_expr(&mut projection.expr, &visible)?;
                }
                Ok(())
            }
            Statement::Notify { .. } => Ok(()),
            _ => Err(SQLError::Internal(
                "validated rewrite-rule action has an unsupported statement kind".into(),
            )),
        }
    }

    fn bind_ctes(
        &mut self,
        ctes: &mut [uqa_sql::ast::CTE],
        inherited: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>, SQLError> {
        let mut visible = inherited.clone();
        let recursive = ctes.iter().any(|cte| cte.recursive).then(|| {
            ctes.iter()
                .map(|cte| cte.name.clone())
                .collect::<BTreeSet<_>>()
        });
        for cte in ctes {
            let body_scope = recursive.as_ref().map_or_else(
                || visible.clone(),
                |recursive| inherited.union(recursive).cloned().collect(),
            );
            self.bind_select(&mut cte.query, &body_scope)?;
            if let Some(cycle) = &mut cte.cycle {
                self.bind_expr(&mut cycle.mark_value, &body_scope)?;
                self.bind_expr(&mut cycle.mark_default, &body_scope)?;
            }
            visible.insert(cte.name.clone());
        }
        Ok(visible)
    }

    fn bind_select(
        &mut self,
        select: &mut SelectStmt,
        inherited: &BTreeSet<String>,
    ) -> Result<(), SQLError> {
        let visible = self.bind_ctes(&mut select.with, inherited)?;
        if let Some(source) = &mut select.from {
            self.bind_from(source, &visible)?;
        }
        for projection in &mut select.projections {
            self.bind_expr(&mut projection.expr, &visible)?;
        }
        for expression in select.values.iter_mut().flatten() {
            self.bind_expr(expression, &visible)?;
        }
        if let Some(expression) = &mut select.r#where {
            self.bind_expr(expression, &visible)?;
        }
        for expression in &mut select.group_by {
            self.bind_expr(expression, &visible)?;
        }
        for expression in select.grouping_sets.iter_mut().flatten() {
            self.bind_expr(expression, &visible)?;
        }
        if let Some(expression) = &mut select.having {
            self.bind_expr(expression, &visible)?;
        }
        for order in &mut select.order_by {
            self.bind_expr(&mut order.expr, &visible)?;
        }
        if let Some(expression) = &mut select.limit {
            self.bind_expr(expression, &visible)?;
        }
        if let Some(expression) = &mut select.offset {
            self.bind_expr(expression, &visible)?;
        }
        for expression in &mut select.distinct_on {
            self.bind_expr(expression, &visible)?;
        }
        if let Some(set) = &mut select.set_op {
            if let Some(left) = &mut set.left {
                self.bind_select(left, &visible)?;
            }
            self.bind_select(&mut set.right, &visible)?;
            for order in &mut set.combined_order_by {
                self.bind_expr(&mut order.expr, &visible)?;
            }
            if let Some(expression) = &mut set.combined_limit {
                self.bind_expr(expression, &visible)?;
            }
            if let Some(expression) = &mut set.combined_offset {
                self.bind_expr(expression, &visible)?;
            }
        }
        Ok(())
    }

    fn bind_from(
        &mut self,
        source: &mut FromClause,
        visible_ctes: &BTreeSet<String>,
    ) -> Result<(), SQLError> {
        match source {
            FromClause::Table { name, .. } => {
                let is_cte = RelationIdentity::parse_reference(name).ok().is_some_and(
                    |(schema, relation)| schema.is_none() && visible_ctes.contains(&relation),
                );
                if !is_cte {
                    (self.visit_relation)(name)?;
                }
            }
            FromClause::Join {
                left, right, on, ..
            } => {
                self.bind_from(left, visible_ctes)?;
                self.bind_from(right, visible_ctes)?;
                if let Some(expression) = on {
                    self.bind_expr(expression, visible_ctes)?;
                }
            }
            FromClause::Values { rows, .. } => {
                for expression in rows.iter_mut().flatten() {
                    self.bind_expr(expression, visible_ctes)?;
                }
            }
            FromClause::Function {
                name,
                binding,
                relations,
                args,
                ..
            } => {
                (self.visit_routine)(name, Some(binding))?;
                if let Some(relations) = relations {
                    (self.visit_relation)(&mut relations.left)?;
                    (self.visit_relation)(&mut relations.right)?;
                }
                for expression in args {
                    self.bind_expr(expression, visible_ctes)?;
                }
            }
            FromClause::FunctionGroup { functions, .. } => {
                for function in functions {
                    (self.visit_routine)(&mut function.name, Some(&mut function.binding))?;
                    if let Some(relations) = &mut function.relations {
                        (self.visit_relation)(&mut relations.left)?;
                        (self.visit_relation)(&mut relations.right)?;
                    }
                    for expression in &mut function.args {
                        self.bind_expr(expression, visible_ctes)?;
                    }
                }
            }
            FromClause::Subquery { body, .. } => self.bind_select(body, visible_ctes)?,
        }
        Ok(())
    }

    fn bind_expr(
        &mut self,
        expression: &mut Expr,
        visible_ctes: &BTreeSet<String>,
    ) -> Result<(), SQLError> {
        match expression {
            Expr::Func {
                name,
                binding,
                args,
                order_by,
                filter,
                ..
            } => {
                for argument in args {
                    self.bind_expr(argument, visible_ctes)?;
                }
                for order in order_by {
                    self.bind_expr(&mut order.expr, visible_ctes)?;
                }
                if let Some(filter) = filter {
                    self.bind_expr(filter, visible_ctes)?;
                }
                (self.visit_routine)(name, Some(binding))?;
            }
            Expr::Array(items) | Expr::Row(items) | Expr::And(items) | Expr::Or(items) => {
                for item in items {
                    self.bind_expr(item, visible_ctes)?;
                }
            }
            Expr::Binary { lhs, rhs, .. } => {
                self.bind_expr(lhs, visible_ctes)?;
                self.bind_expr(rhs, visible_ctes)?;
            }
            Expr::UnaryMinus(inner)
            | Expr::Not(inner)
            | Expr::IsNull { expr: inner, .. }
            | Expr::Cast { expr: inner, .. } => self.bind_expr(inner, visible_ctes)?,
            Expr::Between { expr, low, high } => {
                self.bind_expr(expr, visible_ctes)?;
                self.bind_expr(low, visible_ctes)?;
                self.bind_expr(high, visible_ctes)?;
            }
            Expr::InList { expr, list, .. } => {
                self.bind_expr(expr, visible_ctes)?;
                for item in list {
                    self.bind_expr(item, visible_ctes)?;
                }
            }
            Expr::WindowCall { name, args, spec } => {
                for argument in args {
                    self.bind_expr(argument, visible_ctes)?;
                }
                for partition in &mut spec.partition_by {
                    self.bind_expr(partition, visible_ctes)?;
                }
                for order in &mut spec.order_by {
                    self.bind_expr(&mut order.expr, visible_ctes)?;
                }
                if let Some(frame) = &mut spec.frame {
                    for bound in [&mut frame.start, &mut frame.end] {
                        if let FrameBound::Preceding(inner) | FrameBound::Following(inner) = bound {
                            self.bind_expr(inner, visible_ctes)?;
                        }
                    }
                }
                (self.visit_routine)(name, None)?;
            }
            Expr::Case {
                base,
                when,
                else_branch,
            } => {
                if let Some(base) = base {
                    self.bind_expr(base, visible_ctes)?;
                }
                for (condition, result) in when {
                    self.bind_expr(condition, visible_ctes)?;
                    self.bind_expr(result, visible_ctes)?;
                }
                if let Some(branch) = else_branch {
                    self.bind_expr(branch, visible_ctes)?;
                }
            }
            Expr::ScalarSubquery(body) | Expr::Exists { body, .. } => {
                self.bind_select(body, visible_ctes)?;
            }
            Expr::InSubquery { expr, body, .. } => {
                self.bind_expr(expr, visible_ctes)?;
                self.bind_select(body, visible_ctes)?;
            }
            Expr::Star
            | Expr::QualifiedStar(_)
            | Expr::Default
            | Expr::Column(_)
            | Expr::QualifiedColumn { .. }
            | Expr::InternalColumn(_)
            | Expr::Literal(_)
            | Expr::Param(_) => {}
        }
        Ok(())
    }
}

impl Engine {
    fn bind_rule_relation_reference(
        &self,
        reference: &mut String,
        lookup_mode: RelationLookupMode,
        dependencies: &mut BTreeSet<RelationIdentity>,
    ) -> Result<(), SQLError> {
        if let Some(canonical) =
            crate::engine_session::canonical_virtual_relation_reference(reference)
        {
            *reference = canonical;
            return Ok(());
        }
        if lookup_mode == RelationLookupMode::Dynamic {
            if let Some(canonical) = crate::sql::resolve_age_label_relation_name(self, reference)? {
                let relation = RelationIdentity::from_legacy_name(&canonical).map_err(|error| {
                    SQLError::Internal(format!("decode bound rule source `{canonical}`: {error}"))
                })?;
                *reference = canonical;
                dependencies.insert(relation);
                return Ok(());
            }
        }
        let resolution = match lookup_mode {
            RelationLookupMode::Dynamic => self.resolve_visible_relation_kind(reference)?,
            RelationLookupMode::Bound => self.resolve_bound_relation_kind(reference)?,
        };
        let canonical = match resolution {
            RelationResolution::Found(
                canonical,
                "table" | "view" | "materialized view" | "foreign table" | "sequence",
            ) => canonical,
            RelationResolution::Found(canonical, kind) => {
                return Err(SQLError::Routine {
                    sqlstate: "42809".into(),
                    message: format!(
                        "CREATE RULE source \"{canonical}\" is a {kind}, not a row relation"
                    ),
                });
            }
            RelationResolution::MissingSchema(schema) => {
                return Err(SQLError::Routine {
                    sqlstate: "3F000".into(),
                    message: format!("schema \"{schema}\" does not exist"),
                });
            }
            RelationResolution::MissingRelation => {
                return Err(SQLError::UnknownTable(reference.clone()));
            }
        };
        let relation = RelationIdentity::from_legacy_name(&canonical).map_err(|error| {
            SQLError::Internal(format!("decode bound rule source `{canonical}`: {error}"))
        })?;
        *reference = canonical;
        dependencies.insert(relation);
        Ok(())
    }

    pub(in crate::engine_events) fn bind_rule_action_relation_dependencies(
        &self,
        statement: &mut Statement,
        lookup_mode: RelationLookupMode,
    ) -> Result<RuleDependencies, SQLError> {
        let mut dependencies = BTreeSet::new();
        let mut bind = |reference: &mut String| {
            self.bind_rule_relation_reference(reference, lookup_mode, &mut dependencies)
        };
        let mut ignore_routine = |_: &mut String,
                                  _: Option<&mut Option<uqa_sql::ast::FunctionBinding>>|
         -> Result<(), SQLError> { Ok(()) };
        RuleAstVisitor {
            visit_relation: &mut bind,
            visit_routine: &mut ignore_routine,
        }
        .bind_statement(statement)?;
        Ok(RuleDependencies {
            relations: dependencies,
            columns: BTreeSet::new(),
            routines: BTreeSet::new(),
        })
    }

    pub(in crate::engine_events) fn bind_rule_condition_relation_dependencies(
        &self,
        expression: &mut Expr,
        lookup_mode: RelationLookupMode,
    ) -> Result<RuleDependencies, SQLError> {
        let mut dependencies = BTreeSet::new();
        let mut bind = |reference: &mut String| {
            self.bind_rule_relation_reference(reference, lookup_mode, &mut dependencies)
        };
        let mut ignore_routine = |_: &mut String,
                                  _: Option<&mut Option<uqa_sql::ast::FunctionBinding>>|
         -> Result<(), SQLError> { Ok(()) };
        RuleAstVisitor {
            visit_relation: &mut bind,
            visit_routine: &mut ignore_routine,
        }
        .bind_expr(expression, &BTreeSet::new())?;
        Ok(RuleDependencies {
            relations: dependencies,
            columns: BTreeSet::new(),
            routines: BTreeSet::new(),
        })
    }
}

pub(super) fn rewrite_rule_statement_relation(
    statement: &mut Statement,
    from: &RelationIdentity,
    to: &RelationIdentity,
) -> Result<bool, SQLError> {
    let from = from.qualified_name();
    let to = to.qualified_name();
    let mut changed = false;
    let mut rewrite = |reference: &mut String| {
        if reference == &from {
            reference.clone_from(&to);
            changed = true;
        }
        Ok(())
    };
    let mut ignore_routine = |_: &mut String,
                              _: Option<&mut Option<uqa_sql::ast::FunctionBinding>>|
     -> Result<(), SQLError> { Ok(()) };
    RuleAstVisitor {
        visit_relation: &mut rewrite,
        visit_routine: &mut ignore_routine,
    }
    .bind_statement(statement)?;
    Ok(changed)
}

pub(crate) fn bind_rule_statement_routines(
    statement: &mut Statement,
    references: &[crate::sql::BoundRuleRoutineReference],
) -> Result<bool, SQLError> {
    let mut ignore_relation = |_: &mut String| -> Result<(), SQLError> { Ok(()) };
    let mut references = references.iter();
    let mut changed = false;
    let mut bind =
        |name: &mut String, binding: Option<&mut Option<uqa_sql::ast::FunctionBinding>>| {
            let reference = references.next().ok_or_else(|| {
                SQLError::Internal(format!(
                    "stored rule routine binding has no entry for call `{name}`"
                ))
            })?;
            changed |= apply_routine_reference(name, binding, reference)?;
            Ok(())
        };
    RuleAstVisitor {
        visit_relation: &mut ignore_relation,
        visit_routine: &mut bind,
    }
    .bind_statement(statement)?;
    if let Some(reference) = references.next() {
        return Err(SQLError::Internal(format!(
            "stored rule routine binding entry `{}` has no matching call",
            reference.name
        )));
    }
    Ok(changed)
}

pub(crate) fn rewrite_statement_routine_identity(
    statement: &mut Statement,
    target: &uqa_sql::ast::FunctionBinding,
    new_name: &str,
) -> Result<bool, SQLError> {
    let mut changed = false;
    let mut ignore_relation = |_: &mut String| -> Result<(), SQLError> { Ok(()) };
    let mut rewrite = |name: &mut String,
                       binding: Option<&mut Option<uqa_sql::ast::FunctionBinding>>|
     -> Result<(), SQLError> {
        let Some(binding) = binding.and_then(Option::as_mut) else {
            return Ok(());
        };
        if crate::engine_session::function_binding_matches(binding, target) {
            *name = new_name.to_string();
            binding.name = new_name.to_string();
            changed = true;
        }
        Ok(())
    };
    RuleAstVisitor {
        visit_relation: &mut ignore_relation,
        visit_routine: &mut rewrite,
    }
    .bind_statement(statement)?;
    Ok(changed)
}

pub(crate) fn rewrite_expression_routine_identity(
    expression: &mut Expr,
    target: &uqa_sql::ast::FunctionBinding,
    new_name: &str,
) -> Result<bool, SQLError> {
    let mut changed = false;
    let mut ignore_relation = |_: &mut String| -> Result<(), SQLError> { Ok(()) };
    let mut rewrite = |name: &mut String,
                       binding: Option<&mut Option<uqa_sql::ast::FunctionBinding>>|
     -> Result<(), SQLError> {
        let Some(binding) = binding.and_then(Option::as_mut) else {
            return Ok(());
        };
        if crate::engine_session::function_binding_matches(binding, target) {
            *name = new_name.to_string();
            binding.name = new_name.to_string();
            changed = true;
        }
        Ok(())
    };
    RuleAstVisitor {
        visit_relation: &mut ignore_relation,
        visit_routine: &mut rewrite,
    }
    .bind_expr(expression, &BTreeSet::new())?;
    Ok(changed)
}

pub(super) fn bind_rule_expr_routines(
    expression: &mut Expr,
    references: &[crate::sql::BoundRuleRoutineReference],
) -> Result<(), SQLError> {
    let mut ignore_relation = |_: &mut String| -> Result<(), SQLError> { Ok(()) };
    let mut references = references.iter();
    let mut bind =
        |name: &mut String, binding: Option<&mut Option<uqa_sql::ast::FunctionBinding>>| {
            let reference = references.next().ok_or_else(|| {
                SQLError::Internal(format!(
                    "stored rule routine binding has no entry for call `{name}`"
                ))
            })?;
            apply_routine_reference(name, binding, reference)?;
            Ok(())
        };
    RuleAstVisitor {
        visit_relation: &mut ignore_relation,
        visit_routine: &mut bind,
    }
    .bind_expr(expression, &BTreeSet::new())?;
    if let Some(reference) = references.next() {
        return Err(SQLError::Internal(format!(
            "stored rule routine binding entry `{}` has no matching call",
            reference.name
        )));
    }
    Ok(())
}

fn apply_routine_reference(
    name: &mut String,
    binding: Option<&mut Option<uqa_sql::ast::FunctionBinding>>,
    reference: &crate::sql::BoundRuleRoutineReference,
) -> Result<bool, SQLError> {
    let (_, local_name) = RelationIdentity::parse_reference(name).map_err(|error| {
        SQLError::Internal(format!("decode stored rule routine `{name}`: {error}"))
    })?;
    let (_, reference_local_name) =
        RelationIdentity::parse_reference(&reference.name).map_err(|error| {
            SQLError::Internal(format!(
                "decode bound rule routine `{}`: {error}",
                reference.name
            ))
        })?;
    if local_name != reference_local_name {
        return Err(SQLError::Internal(format!(
            "stored rule routine call `{name}` does not match bound call `{}`",
            reference.name
        )));
    }
    let Some(exact) = &reference.binding else {
        return Ok(false);
    };
    let mut changed = false;
    if !exact.builtin && name != &exact.name {
        name.clone_from(&exact.name);
        changed = true;
    }
    if let Some(binding) = binding {
        if binding.as_ref() != Some(exact) {
            *binding = Some(exact.clone());
            changed = true;
        }
    }
    Ok(changed)
}

pub(super) fn rewrite_stored_rule_relation(
    rule: &mut StoredRule,
    from: &RelationIdentity,
    to: &RelationIdentity,
) -> Result<bool, SQLError> {
    let dependencies = rule.dependencies.as_mut().ok_or_else(|| {
        SQLError::Internal(format!(
            "rule `{}` has no bound dependency state",
            rule.definition.name
        ))
    })?;
    let mut changed = false;
    if dependencies.relations.remove(from) {
        dependencies.relations.insert(to.clone());
        changed = true;
    }
    let renamed_columns = dependencies
        .columns
        .iter()
        .filter(|dependency| &dependency.relation == from)
        .cloned()
        .collect::<Vec<_>>();
    for mut dependency in renamed_columns {
        dependencies.columns.remove(&dependency);
        dependency.relation = to.clone();
        dependencies.columns.insert(dependency);
        changed = true;
    }
    for action in &mut rule.definition.actions {
        changed |= rewrite_rule_statement_relation(action, from, to)?;
    }
    if let Some(condition) = &mut rule.definition.condition {
        let from_name = from.qualified_name();
        let to_name = to.qualified_name();
        let mut rewrite = |reference: &mut String| {
            if reference == &from_name {
                reference.clone_from(&to_name);
                changed = true;
            }
            Ok(())
        };
        let mut ignore_routine = |_: &mut String,
                                  _: Option<&mut Option<uqa_sql::ast::FunctionBinding>>|
         -> Result<(), SQLError> { Ok(()) };
        RuleAstVisitor {
            visit_relation: &mut rewrite,
            visit_routine: &mut ignore_routine,
        }
        .bind_expr(condition, &BTreeSet::new())?;
    }
    if let Some(plan) = &mut rule.condition_plan {
        for subquery in &mut plan.subqueries {
            crate::engine_session::bind_query_plan_relations(
                subquery,
                &BTreeSet::new(),
                &mut |reference| -> Result<String, SQLError> {
                    let identity =
                        RelationIdentity::from_legacy_name(reference).map_err(|error| {
                            SQLError::Internal(format!(
                                "decode stored rule relation `{reference}`: {error}"
                            ))
                        })?;
                    if &identity == from {
                        changed = true;
                        Ok(to.qualified_name())
                    } else {
                        Ok(reference.to_string())
                    }
                },
            )?;
        }
    }
    super::synchronize_rule_sql_text(&mut rule.definition)?;
    Ok(changed)
}

pub(super) fn collect_query_relation_dependencies(
    query: &QueryPlan,
    dependencies: &mut RuleDependencies,
    inherited_ctes: &BTreeSet<String>,
) -> Result<(), SQLError> {
    let mut visible_ctes = inherited_ctes.clone();
    let recursive = query.ctes.iter().any(|cte| cte.recursive).then(|| {
        query
            .ctes
            .iter()
            .map(|cte| cte.name.clone())
            .collect::<BTreeSet<_>>()
    });
    for cte in &query.ctes {
        let body_scope = recursive.as_ref().map_or_else(
            || visible_ctes.clone(),
            |recursive| inherited_ctes.union(recursive).cloned().collect(),
        );
        collect_query_relation_dependencies(&cte.query, dependencies, &body_scope)?;
        visible_ctes.insert(cte.name.clone());
    }
    match &query.root {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = &block.from {
                collect_source_relation_dependencies(source, dependencies, &visible_ctes)?;
            }
            for subquery in &block.subqueries {
                collect_query_relation_dependencies(subquery, dependencies, &visible_ctes)?;
            }
        }
        RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            collect_query_relation_dependencies(left, dependencies, &visible_ctes)?;
            collect_query_relation_dependencies(right, dependencies, &visible_ctes)?;
            for subquery in subqueries {
                collect_query_relation_dependencies(subquery, dependencies, &visible_ctes)?;
            }
        }
        RelationalPlan::Values { subqueries, .. } => {
            for subquery in subqueries {
                collect_query_relation_dependencies(subquery, dependencies, &visible_ctes)?;
            }
        }
    }
    Ok(())
}

pub(super) fn collect_expression_routine_dependencies(
    expression: &uqa_planner::ExpressionPlan,
    dependencies: &mut RuleDependencies,
) {
    let mut scalar = expression.scalar.clone();
    uqa_planner::rewrite_scalar_expression(&mut scalar, &mut |expression| {
        if let uqa_execution::ScalarExpr::Func {
            binding: Some(binding),
            ..
        } = expression
        {
            insert_routine_dependency(binding, dependencies);
        }
    });
    for query in &expression.subqueries {
        collect_query_routine_dependencies(query, dependencies);
    }
}

pub(super) fn collect_query_routine_dependencies(
    query: &QueryPlan,
    dependencies: &mut RuleDependencies,
) {
    let mut scalar_plan = query.clone();
    scalar_plan.rewrite_scalar_expressions(&mut |expression| {
        if let uqa_execution::ScalarExpr::Func {
            binding: Some(binding),
            ..
        } = expression
        {
            insert_routine_dependency(binding, dependencies);
        }
    });
    for cte in &query.ctes {
        collect_query_source_routine_dependencies(&cte.query, dependencies);
    }
    collect_relational_source_routine_dependencies(&query.root, dependencies);
}

fn collect_query_source_routine_dependencies(
    query: &QueryPlan,
    dependencies: &mut RuleDependencies,
) {
    for cte in &query.ctes {
        collect_query_source_routine_dependencies(&cte.query, dependencies);
    }
    collect_relational_source_routine_dependencies(&query.root, dependencies);
}

fn collect_relational_source_routine_dependencies(
    plan: &RelationalPlan,
    dependencies: &mut RuleDependencies,
) {
    match plan {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = &block.from {
                collect_source_routine_dependencies(source, dependencies);
            }
            for subquery in &block.subqueries {
                collect_query_source_routine_dependencies(subquery, dependencies);
            }
        }
        RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            collect_query_source_routine_dependencies(left, dependencies);
            collect_query_source_routine_dependencies(right, dependencies);
            for subquery in subqueries {
                collect_query_source_routine_dependencies(subquery, dependencies);
            }
        }
        RelationalPlan::Values { subqueries, .. } => {
            for subquery in subqueries {
                collect_query_source_routine_dependencies(subquery, dependencies);
            }
        }
    }
}

fn collect_source_routine_dependencies(source: &SourcePlan, dependencies: &mut RuleDependencies) {
    match source {
        SourcePlan::Table { .. } | SourcePlan::Values { .. } => {}
        SourcePlan::Join { left, right, .. } => {
            collect_source_routine_dependencies(left, dependencies);
            collect_source_routine_dependencies(right, dependencies);
        }
        SourcePlan::Subquery { body, .. } => {
            collect_query_source_routine_dependencies(body, dependencies);
        }
        SourcePlan::Function { binding, .. } => {
            if let Some(binding) = binding {
                insert_routine_dependency(binding, dependencies);
            }
        }
        SourcePlan::FunctionGroup { functions, .. } => {
            for function in functions {
                if let Some(binding) = &function.binding {
                    insert_routine_dependency(binding, dependencies);
                }
            }
        }
    }
}

fn insert_routine_dependency(
    binding: &uqa_sql::ast::FunctionBinding,
    dependencies: &mut RuleDependencies,
) {
    if !binding.builtin {
        dependencies.routines.insert(RuleRoutineDependency {
            object_id: binding.object_id,
            name: binding.name.clone(),
            argument_types: binding.argument_types.clone(),
        });
    }
}

fn collect_source_relation_dependencies(
    source: &SourcePlan,
    dependencies: &mut RuleDependencies,
    visible_ctes: &BTreeSet<String>,
) -> Result<(), SQLError> {
    match source {
        SourcePlan::Table { name, .. } => {
            if crate::engine_session::canonical_virtual_relation_reference(name).is_some() {
                return Ok(());
            }
            let (schema, relation) = RelationIdentity::parse_reference(name).map_err(|error| {
                SQLError::Internal(format!("decode stored rule dependency `{name}`: {error}"))
            })?;
            if schema.is_none() && visible_ctes.contains(&relation) {
                return Ok(());
            }
            let schema = schema.ok_or_else(|| {
                SQLError::Internal(format!(
                    "stored rule relation dependency `{name}` is not catalog-bound"
                ))
            })?;
            dependencies
                .relations
                .insert(RelationIdentity::new(schema, relation));
        }
        SourcePlan::Join { left, right, .. } => {
            collect_source_relation_dependencies(left, dependencies, visible_ctes)?;
            collect_source_relation_dependencies(right, dependencies, visible_ctes)?;
        }
        SourcePlan::Subquery { body, .. } => {
            collect_query_relation_dependencies(body, dependencies, visible_ctes)?;
        }
        SourcePlan::Function { relations, .. } => {
            if let Some(relations) = relations {
                collect_canonical_relation(&relations.left, dependencies)?;
                collect_canonical_relation(&relations.right, dependencies)?;
            }
        }
        SourcePlan::FunctionGroup { functions, .. } => {
            for function in functions {
                if let Some(relations) = &function.relations {
                    collect_canonical_relation(&relations.left, dependencies)?;
                    collect_canonical_relation(&relations.right, dependencies)?;
                }
            }
        }
        SourcePlan::Values { .. } => {}
    }
    Ok(())
}

fn collect_canonical_relation(
    reference: &str,
    dependencies: &mut RuleDependencies,
) -> Result<(), SQLError> {
    let relation = RelationIdentity::from_legacy_name(reference).map_err(|error| {
        SQLError::Internal(format!(
            "decode stored rule dependency `{reference}`: {error}"
        ))
    })?;
    dependencies.relations.insert(relation);
    Ok(())
}
