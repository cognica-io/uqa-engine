//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable catalog AST binding and dependency traversal.

mod ctes;
use ctes::{collect_cte_relation_dependencies, collect_cte_source_routine_dependencies};
pub(crate) use uqa_sql::catalog::stored_ast::visit_stored_statement_merges;
pub(crate) use uqa_sql::catalog::stored_ast::{
    copy_stored_source_column_shapes, visit_stored_statement_sources,
};
pub(crate) use uqa_sql::catalog::stored_ast::{
    stored_expression_type_names, stored_statement_relation_names, stored_statement_type_names,
};
pub(crate) use uqa_sql::catalog::stored_ast::{
    visit_stored_expression, visit_stored_statement_expressions,
};

use std::collections::BTreeSet;

use uqa_planner::{QueryPlan, RelationalPlan, SourcePlan};
use uqa_sql::ast::{Expr, Statement};
use uqa_sql::catalog::stored_ast::StoredAstVisitor;
use uqa_sql::SQLError;

use crate::capabilities::{RelationLookupMode, RelationResolution};
use crate::{Engine, RelationIdentity};

use super::{RuleDependencies, RuleRoutineDependency};

pub(crate) use uqa_sql::catalog::stored_ast::{
    bind_stored_expression_routines, bind_stored_statement_routines,
    expression_references_routine_identity, rewrite_expression_routine_identity,
    rewrite_statement_routine_identity, statement_references_routine_identity,
};

impl Engine {
    fn bind_catalog_relation_reference(
        &self,
        reference: &mut String,
        lookup_mode: RelationLookupMode,
        loaded_catalog: bool,
        context: &str,
        dependencies: &mut BTreeSet<RelationIdentity>,
    ) -> Result<(), SQLError> {
        if let Some(canonical) = crate::session::canonical_virtual_relation_reference(reference) {
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

    pub(in crate::events) fn bind_rule_action_relation_dependencies(
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
            source: None,
            merge: None,
            expression: None,
            ty: None,
            relation: &mut bind,
            routine: &mut ignore_routine,
        }
        .bind_statement(statement)?;
        Ok(RuleDependencies {
            relations: dependencies,
            columns: BTreeSet::new(),
            routines: BTreeSet::new(),
        })
    }

    pub(in crate::events) fn bind_rule_condition_relation_dependencies(
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
            source: None,
            merge: None,
            expression: None,
            ty: None,
            relation: &mut bind,
            routine: &mut ignore_routine,
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
            source: None,
            merge: None,
            expression: None,
            ty: None,
            relation: &mut bind,
            routine: &mut ignore_routine,
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

pub(crate) use uqa_sql::catalog::events::renames::rewrite_stored_statement_relation;

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
        collect_cte_relation_dependencies(&cte.body, dependencies, &body_scope)?;
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
        collect_cte_source_routine_dependencies(&cte.body, dependencies);
    }
    collect_relational_source_routine_dependencies(&query.root, dependencies);
}

fn collect_query_source_routine_dependencies(
    query: &QueryPlan,
    dependencies: &mut RuleDependencies,
) {
    for cte in &query.ctes {
        collect_cte_source_routine_dependencies(&cte.body, dependencies);
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
            if crate::session::canonical_virtual_relation_reference(name).is_some() {
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
