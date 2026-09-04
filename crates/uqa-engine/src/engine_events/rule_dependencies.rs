//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable catalog AST binding and dependency traversal.

mod routines;

use std::collections::BTreeSet;

use uqa_planner::{QueryPlan, RelationalPlan, SourcePlan};
use uqa_sql::ast::{Expr, FrameBound, FromClause, SelectStmt, Statement};
use uqa_sql::SQLError;

use crate::engine_capabilities::{RelationLookupMode, RelationResolution};
use crate::{Engine, RelationIdentity};

use super::{RuleDependencies, RuleRoutineDependency, StoredRule};

pub(crate) use routines::{
    bind_stored_expression_routines, bind_stored_statement_routines,
    expression_references_routine_identity, rewrite_expression_routine_identity,
    rewrite_statement_routine_identity, statement_references_routine_identity,
};

struct StoredAstVisitor<'a, R, F> {
    visit_relation: &'a mut R,
    visit_routine: &'a mut F,
}

impl<R, F> StoredAstVisitor<'_, R, F>
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
            Statement::Insert(insert) => self.bind_insert(insert, &ctes),
            Statement::Update(update) => self.bind_update(update, &ctes),
            Statement::Delete(delete) => self.bind_delete(delete, &ctes),
            Statement::Notify { .. } => Ok(()),
            Statement::Values { rows } => {
                for expression in rows.iter_mut().flatten() {
                    self.bind_expr(expression, &ctes)?;
                }
                Ok(())
            }
            Statement::Merge(merge) => self.bind_merge(merge, &ctes),
            _ => Err(SQLError::Internal(
                "catalog-owned statement has an unsupported dependency shape".into(),
            )),
        }
    }

    fn bind_insert(
        &mut self,
        insert: &mut uqa_sql::ast::InsertStmt,
        inherited: &BTreeSet<String>,
    ) -> Result<(), SQLError> {
        (self.visit_relation)(&mut insert.table)?;
        let visible = self.bind_ctes(&mut insert.with, inherited)?;
        if let Some(source) = insert.select_source.as_deref_mut() {
            self.bind_select(source, &visible)?;
        }
        for expression in insert.rows.iter_mut().flatten() {
            self.bind_expr(expression, &visible)?;
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

    fn bind_update(
        &mut self,
        update: &mut uqa_sql::ast::UpdateStmt,
        inherited: &BTreeSet<String>,
    ) -> Result<(), SQLError> {
        (self.visit_relation)(&mut update.table)?;
        let visible = self.bind_ctes(&mut update.with, inherited)?;
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

    fn bind_delete(
        &mut self,
        delete: &mut uqa_sql::ast::DeleteStmt,
        inherited: &BTreeSet<String>,
    ) -> Result<(), SQLError> {
        (self.visit_relation)(&mut delete.table)?;
        let visible = self.bind_ctes(&mut delete.with, inherited)?;
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

    fn bind_merge(
        &mut self,
        merge: &mut uqa_sql::ast::MergeStmt,
        ctes: &BTreeSet<String>,
    ) -> Result<(), SQLError> {
        (self.visit_relation)(&mut merge.target)?;
        self.bind_from(&mut merge.source, ctes)?;
        self.bind_expr(&mut merge.join_condition, ctes)?;
        for clause in &mut merge.when_clauses {
            match clause {
                uqa_sql::ast::MergeWhen::UpdateMatched {
                    condition,
                    assignments,
                }
                | uqa_sql::ast::MergeWhen::UpdateNotMatchedBySource {
                    condition,
                    assignments,
                } => {
                    if let Some(condition) = condition {
                        self.bind_expr(condition, ctes)?;
                    }
                    for (_, expression) in assignments {
                        self.bind_expr(expression, ctes)?;
                    }
                }
                uqa_sql::ast::MergeWhen::InsertNotMatched {
                    condition, values, ..
                } => {
                    if let Some(condition) = condition {
                        self.bind_expr(condition, ctes)?;
                    }
                    for expression in values {
                        self.bind_expr(expression, ctes)?;
                    }
                }
                uqa_sql::ast::MergeWhen::DeleteMatched { condition }
                | uqa_sql::ast::MergeWhen::DeleteNotMatchedBySource { condition }
                | uqa_sql::ast::MergeWhen::NothingMatched { condition }
                | uqa_sql::ast::MergeWhen::NothingNotMatched { condition }
                | uqa_sql::ast::MergeWhen::NothingNotMatchedBySource { condition } => {
                    if let Some(condition) = condition {
                        self.bind_expr(condition, ctes)?;
                    }
                }
            }
        }
        for projection in &mut merge.returning {
            self.bind_expr(&mut projection.expr, ctes)?;
        }
        Ok(())
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
    fn bind_catalog_relation_reference(
        &self,
        reference: &mut String,
        lookup_mode: RelationLookupMode,
        loaded_catalog: bool,
        context: &str,
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
        let resolution = match (lookup_mode, loaded_catalog) {
            (RelationLookupMode::Dynamic, true) => {
                self.resolve_loaded_visible_relation_kind(reference)?
            }
            (RelationLookupMode::Dynamic, false) => {
                self.resolve_visible_relation_kind(reference)?
            }
            (RelationLookupMode::Bound, _) => self.resolve_bound_relation_kind(reference)?,
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
                        "{context} source \"{canonical}\" is a {kind}, not a row relation"
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
            self.bind_catalog_relation_reference(
                reference,
                lookup_mode,
                false,
                "CREATE RULE",
                &mut dependencies,
            )
        };
        let mut ignore_routine = |_: &mut String,
                                  _: Option<&mut Option<uqa_sql::ast::FunctionBinding>>|
         -> Result<(), SQLError> { Ok(()) };
        StoredAstVisitor {
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
            self.bind_catalog_relation_reference(
                reference,
                lookup_mode,
                false,
                "CREATE RULE",
                &mut dependencies,
            )
        };
        let mut ignore_routine = |_: &mut String,
                                  _: Option<&mut Option<uqa_sql::ast::FunctionBinding>>|
         -> Result<(), SQLError> { Ok(()) };
        StoredAstVisitor {
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

    pub(crate) fn bind_stored_statement_relations(
        &self,
        statement: &mut Statement,
        lookup_mode: RelationLookupMode,
        loaded_catalog: bool,
        context: &str,
    ) -> Result<bool, SQLError> {
        let mut dependencies = BTreeSet::new();
        let mut changed = false;
        let mut bind = |reference: &mut String| {
            let previous = reference.clone();
            self.bind_catalog_relation_reference(
                reference,
                lookup_mode,
                loaded_catalog,
                context,
                &mut dependencies,
            )?;
            changed |= reference != &previous;
            Ok(())
        };
        let mut ignore_routine = |_: &mut String,
                                  _: Option<&mut Option<uqa_sql::ast::FunctionBinding>>|
         -> Result<(), SQLError> { Ok(()) };
        StoredAstVisitor {
            visit_relation: &mut bind,
            visit_routine: &mut ignore_routine,
        }
        .bind_statement(statement)?;
        match statement {
            Statement::Insert(insert) => {
                changed |= !insert.target_relation_bound;
                insert.target_relation_bound = true;
            }
            Statement::Update(update) => {
                changed |= !update.target_relation_bound;
                update.target_relation_bound = true;
            }
            Statement::Delete(delete) => {
                changed |= !delete.target_relation_bound;
                delete.target_relation_bound = true;
            }
            _ => {}
        }
        Ok(changed)
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
    StoredAstVisitor {
        visit_relation: &mut rewrite,
        visit_routine: &mut ignore_routine,
    }
    .bind_statement(statement)?;
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
        StoredAstVisitor {
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
