//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored-view relation and sequence binding/dependency analysis.

use super::{QueryPlan, RelationIdentity, RelationalPlan, ScalarExpr, SourcePlan, Value};

pub(super) fn canonical_virtual_relation_reference(reference: &str) -> Option<String> {
    let (schema, relation) = RelationIdentity::parse_reference(reference).ok()?;
    let relation = relation.to_ascii_lowercase();
    let schema = schema.map(|schema| schema.to_ascii_lowercase());
    let information_schema = matches!(
        relation.as_str(),
        "schemata"
            | "tables"
            | "columns"
            | "views"
            | "routines"
            | "sequences"
            | "table_constraints"
            | "key_column_usage"
    );
    let pg_catalog = matches!(
        relation.as_str(),
        "pg_namespace"
            | "pg_class"
            | "pg_attribute"
            | "pg_attrdef"
            | "pg_constraint"
            | "pg_index"
            | "pg_tables"
            | "pg_views"
            | "pg_indexes"
            | "pg_type"
            | "pg_proc"
            | "pg_database"
            | "pg_roles"
            | "pg_user"
            | "pg_settings"
            | "pg_description"
            | "pg_matviews"
            | "pg_sequences"
    );
    match schema.as_deref() {
        Some("information_schema") if information_schema => {
            Some(format!("information_schema.{relation}"))
        }
        Some("pg_catalog") | None if pg_catalog => Some(format!("pg_catalog.{relation}")),
        _ => None,
    }
}

pub(super) fn sequence_function_reference_mut(expression: &mut ScalarExpr) -> Option<&mut String> {
    let ScalarExpr::Func { name, args, .. } = expression else {
        return None;
    };
    let lower = name.to_ascii_lowercase();
    let local = lower.strip_prefix("pg_catalog.").unwrap_or(&lower);
    if !matches!(local, "nextval" | "currval" | "setval")
        || (lower.contains('.') && !lower.starts_with("pg_catalog."))
    {
        return None;
    }
    regclass_literal_mut(args.first_mut()?)
}

pub(super) fn regclass_literal_mut(expression: &mut ScalarExpr) -> Option<&mut String> {
    match expression {
        ScalarExpr::Literal(Value::Str(reference)) => Some(reference),
        ScalarExpr::Cast { expr, ty }
            if ty
                .rsplit_once('.')
                .map_or(ty.as_str(), |(_, local)| local)
                .eq_ignore_ascii_case("regclass") =>
        {
            regclass_literal_mut(expr)
        }
        _ => None,
    }
}

pub(super) fn bind_query_plan_sequence_references<E>(
    plan: &mut QueryPlan,
    resolve: &mut impl FnMut(&str) -> Result<String, E>,
) -> Result<(), E> {
    let mut error = None;
    plan.rewrite_scalar_expressions(&mut |expression| {
        if error.is_some() {
            return;
        }
        let Some(reference) = sequence_function_reference_mut(expression) else {
            return;
        };
        match resolve(reference) {
            Ok(canonical) => *reference = canonical,
            Err(binding_error) => error = Some(binding_error),
        }
    });
    error.map_or(Ok(()), Err)
}

pub(super) fn bind_query_plan_relations<E>(
    plan: &mut QueryPlan,
    inherited_ctes: &std::collections::BTreeSet<String>,
    resolve: &mut impl FnMut(&str) -> Result<String, E>,
) -> Result<(), E> {
    // Non-recursive CTEs see outer and preceding CTEs. A recursive CTE also
    // sees its own name while its body is bound. This mirrors materialization
    // order and prevents a real table with the same local name from being
    // mistaken for a CTE (or vice versa).
    let mut visible_ctes = inherited_ctes.clone();
    for cte in &mut plan.ctes {
        let mut body_ctes = visible_ctes.clone();
        if cte.recursive {
            body_ctes.insert(cte.name.clone());
        }
        bind_query_plan_relations(&mut cte.query, &body_ctes, resolve)?;
        visible_ctes.insert(cte.name.clone());
    }
    bind_relational_plan_relations(&mut plan.root, &visible_ctes, resolve)
}

pub(super) fn bind_relational_plan_relations<E>(
    plan: &mut RelationalPlan,
    visible_ctes: &std::collections::BTreeSet<String>,
    resolve: &mut impl FnMut(&str) -> Result<String, E>,
) -> Result<(), E> {
    match plan {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = &mut block.from {
                bind_source_plan_relations(source, visible_ctes, resolve)?;
            }
            for subquery in &mut block.subqueries {
                bind_query_plan_relations(subquery, visible_ctes, resolve)?;
            }
        }
        RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            bind_query_plan_relations(left, visible_ctes, resolve)?;
            bind_query_plan_relations(right, visible_ctes, resolve)?;
            for subquery in subqueries {
                bind_query_plan_relations(subquery, visible_ctes, resolve)?;
            }
        }
        RelationalPlan::Values { subqueries, .. } => {
            for subquery in subqueries {
                bind_query_plan_relations(subquery, visible_ctes, resolve)?;
            }
        }
    }
    Ok(())
}

pub(super) fn bind_source_plan_relations<E>(
    source: &mut SourcePlan,
    visible_ctes: &std::collections::BTreeSet<String>,
    resolve: &mut impl FnMut(&str) -> Result<String, E>,
) -> Result<(), E> {
    match source {
        SourcePlan::Table { name, .. } => {
            let is_cte =
                RelationIdentity::parse_reference(name)
                    .ok()
                    .is_some_and(|(schema, relation)| {
                        schema.is_none() && visible_ctes.contains(&relation)
                    });
            if !is_cte {
                *name = resolve(name)?;
            }
        }
        SourcePlan::Join { left, right, .. } => {
            bind_source_plan_relations(left, visible_ctes, resolve)?;
            bind_source_plan_relations(right, visible_ctes, resolve)?;
        }
        SourcePlan::Subquery { body, .. } => {
            bind_query_plan_relations(body, visible_ctes, resolve)?;
        }
        SourcePlan::Function {
            relation: Some(relation),
            ..
        } => {
            *relation = resolve(relation)?;
        }
        SourcePlan::Values { .. } | SourcePlan::Function { relation: None, .. } => {}
    }
    Ok(())
}

pub(super) fn relation_reference_matches(reference: &str, target: &RelationIdentity) -> bool {
    match RelationIdentity::parse_reference(reference) {
        Ok((Some(schema), name)) => schema == target.schema && name == target.name,
        Ok((None, name)) => name == target.name,
        // A malformed stored plan must fail closed: treating it as unrelated
        // could permit DDL to leave an unexecutable view behind.
        Err(_) => true,
    }
}

pub(super) fn source_plan_references_relation(
    source: &uqa_planner::SourcePlan,
    target: &RelationIdentity,
    ctes: &std::collections::BTreeSet<String>,
) -> bool {
    match source {
        uqa_planner::SourcePlan::Table { name, .. } => {
            let is_cte = RelationIdentity::parse_reference(name)
                .ok()
                .is_some_and(|(schema, relation)| schema.is_none() && ctes.contains(&relation));
            !is_cte && relation_reference_matches(name, target)
        }
        uqa_planner::SourcePlan::Join { left, right, .. } => {
            source_plan_references_relation(left, target, ctes)
                || source_plan_references_relation(right, target, ctes)
        }
        uqa_planner::SourcePlan::Subquery { body, .. } => {
            query_plan_references_relation(body, target, ctes)
        }
        uqa_planner::SourcePlan::Function { relation, .. } => relation
            .as_ref()
            .is_some_and(|relation| relation_reference_matches(relation, target)),
        uqa_planner::SourcePlan::Values { .. } => false,
    }
}

pub(super) fn query_plan_references_relation(
    query: &uqa_planner::QueryPlan,
    target: &RelationIdentity,
    inherited_ctes: &std::collections::BTreeSet<String>,
) -> bool {
    let mut ctes = inherited_ctes.clone();
    ctes.extend(query.ctes.iter().map(|cte| cte.name.clone()));
    if query
        .ctes
        .iter()
        .any(|cte| query_plan_references_relation(&cte.query, target, &ctes))
    {
        return true;
    }
    match &query.root {
        uqa_planner::RelationalPlan::QueryBlock(block) => {
            block
                .from
                .as_ref()
                .is_some_and(|source| source_plan_references_relation(source, target, &ctes))
                || block
                    .subqueries
                    .iter()
                    .any(|query| query_plan_references_relation(query, target, &ctes))
        }
        uqa_planner::RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            query_plan_references_relation(left, target, &ctes)
                || query_plan_references_relation(right, target, &ctes)
                || subqueries
                    .iter()
                    .any(|query| query_plan_references_relation(query, target, &ctes))
        }
        uqa_planner::RelationalPlan::Values { subqueries, .. } => subqueries
            .iter()
            .any(|query| query_plan_references_relation(query, target, &ctes)),
    }
}

pub(super) fn query_plan_references_sequence(plan: &QueryPlan, target: &RelationIdentity) -> bool {
    let mut plan = plan.clone();
    let mut referenced = false;
    plan.rewrite_scalar_expressions(&mut |expression| {
        if let Some(reference) = sequence_function_reference_mut(expression) {
            referenced |= relation_reference_matches(reference, target);
        }
    });
    referenced
}
