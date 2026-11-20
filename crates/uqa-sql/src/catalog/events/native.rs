//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read original event ownership and concrete dependencies without rebinding stored definitions.

use super::{definition::EventAnalysisContext, RuleCatalog, StoredRule, TriggerCatalog};
use crate::{
    binding::view_dependencies::visit_stored_plan_relation_references_with_work,
    catalog::{resolution::bound_relation_identity_with_control, work::Work},
    plan::{ExpressionPlan, PlanRef, UnifiedPlan},
    SQLError,
};
use uqa_core::{memory::ProductionControl, RelationIdentity};

pub mod bindings;
pub mod dependencies;

/// Read the selected immutable catalog. Implementations supply existing native layouts and the ordinary virtual/graph metadata; they do not compile stored SQL or resolve a new search path.
pub trait NativeEventCatalog {
    fn kind(&self, relation: &RelationIdentity) -> Result<Option<&'static str>, SQLError>;
    fn contains_column(
        &self,
        relation: &RelationIdentity,
        column: &str,
        control: &ProductionControl<'_>,
    ) -> Result<bool, SQLError>;
}

pub fn validate(
    triggers: &TriggerCatalog,
    rules: &RuleCatalog,
    catalog: &impl NativeEventCatalog,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    crate::catalog::work::inline(validate_with_work(
        triggers,
        rules,
        catalog,
        control,
        &mut (),
    ))
}

/// Inspect stored events in catalog order using the caller's work window.
pub async fn validate_with_work<W: Work + Send>(
    triggers: &TriggerCatalog,
    rules: &RuleCatalog,
    catalog: &impl NativeEventCatalog,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<(), SQLError> {
    control.check()?;
    for (owner, definitions) in triggers {
        work.checkpoint().await?;
        for (name, trigger) in definitions {
            work.checkpoint().await?;
            control.check()?;
            let definition = &trigger.definition;
            declaration(owner, name, &definition.table, &definition.name, control)?;
            let kind = relation(catalog, owner, control)?;
            EventAnalysisContext::validate_trigger_relation_kind(definition, owner, kind)?;
            if let Some(reference) = &definition.referenced_table {
                let referenced = bound_relation_identity_with_control(reference, control)?;
                if relation(catalog, &referenced, control)? != "table" {
                    return Err(invalid("constraint-trigger referenced relation kind"));
                }
            }
            for column in &definition.update_columns {
                work.checkpoint().await?;
                live_column(catalog, owner, column, control)?;
            }
        }
    }
    for (owner, definitions) in rules {
        work.checkpoint().await?;
        for (name, rule) in definitions {
            work.checkpoint().await?;
            control.check()?;
            declaration(
                owner,
                name,
                &rule.definition.table,
                &rule.definition.name,
                control,
            )?;
            if !matches!(relation(catalog, owner, control)?, "table" | "view") {
                return Err(invalid("rule owning relation kind"));
            }
            condition(rule, owner, catalog, control, work).await?;
            if let Some(dependencies) = &rule.dependencies {
                for dependency in &dependencies.relations {
                    work.checkpoint().await?;
                    relation(catalog, dependency, control)?;
                }
                for dependency in &dependencies.columns {
                    work.checkpoint().await?;
                    live_column(catalog, &dependency.relation, &dependency.column, control)?;
                }
            }
            visit_rule_relations_with_work(rule, control, work, &mut |reference, _| {
                let identity = bound_relation_identity_with_control(reference, control)?;
                relation(catalog, &identity, control).map(|_| ())
            })
            .await?;
        }
    }
    control.check()?;
    Ok(())
}

fn declaration(
    owner: &RelationIdentity,
    name: &str,
    stored_owner: &str,
    stored_name: &str,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let reference = bound_relation_identity_with_control(stored_owner, control)?;
    if &*reference != owner || name != stored_name {
        return Err(invalid("declaration differs from its registry key"));
    }
    Ok(())
}

fn relation(
    catalog: &impl NativeEventCatalog,
    reference: &RelationIdentity,
    control: &ProductionControl<'_>,
) -> Result<&'static str, SQLError> {
    control.check()?;
    catalog
        .kind(reference)?
        .ok_or_else(|| invalid("missing relation"))
}

fn live_column(
    catalog: &impl NativeEventCatalog,
    reference: &RelationIdentity,
    column: &str,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    relation(catalog, reference, control)?;
    if !catalog.contains_column(reference, column, control)? {
        return Err(invalid("missing column"));
    }
    control.check()?;
    Ok(())
}

async fn condition(
    rule: &StoredRule,
    owner: &RelationIdentity,
    catalog: &impl NativeEventCatalog,
    control: &ProductionControl<'_>,
    work: &mut (impl Work + Send),
) -> Result<(), SQLError> {
    if let Some((plan, binding)) = condition_parts(rule)? {
        binding
            .validate_captured_with_work(
                rule.definition.event,
                plan,
                control,
                work,
                &mut |column| live_column(catalog, owner, column, control),
            )
            .await?;
    }
    Ok(())
}

fn condition_parts(
    rule: &StoredRule,
) -> Result<Option<(&ExpressionPlan, &super::RuleConditionBinding)>, SQLError> {
    match (
        &rule.definition.condition,
        &rule.condition_plan,
        &rule.condition_binding,
    ) {
        (None, None, None) => Ok(None),
        (Some(_), Some(plan), Some(binding)) => Ok(Some((plan, binding))),
        _ => Err(invalid("incomplete captured rule condition")),
    }
}

/// Reuse native plan traversal for nested/mutation CTEs and fixed condition subqueries. OLD/NEW scalar references name no relation source; FROM items still require ordinary CTE or catalog resolution. Missing legacy dependency summaries cannot hide concrete action targets.
pub fn visit_rule_relations(
    rule: &StoredRule,
    control: &ProductionControl<'_>,
    visit: &mut impl FnMut(&str, bool) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    crate::catalog::work::inline(visit_rule_relations_with_work(
        rule,
        control,
        &mut (),
        visit,
    ))
}

async fn visit_rule_relations_with_work<W: Work + Send>(
    rule: &StoredRule,
    control: &ProductionControl<'_>,
    work: &mut W,
    visit: &mut impl FnMut(&str, bool) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    for action in &rule.definition.actions {
        work.checkpoint().await?;
        control.check()?;
        let plan = UnifiedPlan::lower_stored_statement_with_work(action, control, work).await?;
        visit_stored_plan_relation_references_with_work(plan.as_plan_ref(), control, work, visit)
            .await?;
        plan.retire(control, work).await?;
    }
    if let Some(expression) = &rule.definition.condition {
        let condition = ExpressionPlan::lower_borrowed_for_rewrite(expression, control)?;
        for query in condition.queries() {
            visit_stored_plan_relation_references_with_work(
                PlanRef::Query(query),
                control,
                work,
                visit,
            )
            .await?;
        }
    }
    if let Some(condition) = &rule.condition_plan {
        for query in &condition.subqueries {
            visit_stored_plan_relation_references_with_work(
                PlanRef::Query(query),
                control,
                work,
                visit,
            )
            .await?;
        }
    }
    control.check()?;
    Ok(())
}

fn invalid(detail: &str) -> SQLError {
    SQLError::Internal(format!("invalid native event reference: {detail}"))
}

#[cfg(test)]
mod tests;
