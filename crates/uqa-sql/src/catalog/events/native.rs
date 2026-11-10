//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read original event ownership and concrete dependencies without rebinding stored definitions.

use super::{definition::EventAnalysisContext, RuleCatalog, StoredRule, TriggerCatalog};
use crate::{
    binding::view_dependencies::visit_stored_plan_relation_references,
    catalog::resolution::bound_relation_identity_with_control,
    plan::{ExpressionPlan, PlanRef, UnifiedPlan},
    SQLError,
};
use uqa_core::{memory::ProductionControl, RelationIdentity};

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
    control.check()?;
    for (owner, definitions) in triggers {
        for (name, trigger) in definitions {
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
                live_column(catalog, owner, column, control)?;
            }
        }
    }
    for (owner, definitions) in rules {
        for (name, rule) in definitions {
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
            if let Some(dependencies) = &rule.dependencies {
                for dependency in &dependencies.relations {
                    relation(catalog, dependency, control)?;
                }
                for dependency in &dependencies.columns {
                    live_column(catalog, &dependency.relation, &dependency.column, control)?;
                }
            }
            visit_rule_relations(rule, control, &mut |reference, _| {
                let identity = bound_relation_identity_with_control(reference, control)?;
                relation(catalog, &identity, control).map(|_| ())
            })?;
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

/// Reuse native plan traversal for nested/mutation CTEs and fixed condition subqueries. OLD/NEW scalar references name no relation source; FROM items still require ordinary CTE or catalog resolution. Missing legacy dependency summaries cannot hide concrete action targets.
pub fn visit_rule_relations(
    rule: &StoredRule,
    control: &ProductionControl<'_>,
    visit: &mut impl FnMut(&str, bool) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    for action in &rule.definition.actions {
        control.check()?;
        let plan = UnifiedPlan::lower_borrowed_for_rewrite(action, &|_: &str| false, control)?;
        visit_stored_plan_relation_references(plan.as_plan_ref(), control, visit)?;
    }
    if let Some(expression) = &rule.definition.condition {
        let condition = ExpressionPlan::lower_borrowed_for_rewrite(expression, control)?;
        for query in condition.queries() {
            visit_stored_plan_relation_references(PlanRef::Query(query), control, visit)?;
        }
    }
    if let Some(condition) = &rule.condition_plan {
        for query in &condition.subqueries {
            visit_stored_plan_relation_references(PlanRef::Query(query), control, visit)?;
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
