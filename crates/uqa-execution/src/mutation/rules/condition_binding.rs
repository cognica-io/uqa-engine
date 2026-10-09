//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    BTreeMap, ProjectedRuntimeRuleResolver, RuleColumnMetadata, RuleContext, RuleRowImage,
    RuleRowSide, SQLError, Value,
};
use uqa_sql::catalog::roles::RoleReference;

fn materialize_rule_condition_row<F>(
    binding: &uqa_sql::catalog::events::RuleConditionBinding,
    required_columns: &std::collections::BTreeSet<String>,
    resolver: &mut ProjectedRuntimeRuleResolver<'_, F>,
) -> Result<(crate::RowSchema, crate::PhysicalRow), SQLError>
where
    F: FnMut(usize, RuleRowSide, &str) -> Result<Option<Value>, SQLError>,
{
    let mut names = Vec::with_capacity(resolver.columns.len() * 2);
    let mut identities = Vec::with_capacity(resolver.columns.len() * 2);
    let mut types = Vec::with_capacity(resolver.columns.len() * 2);
    let mut values = Vec::with_capacity(resolver.columns.len() * 2);
    let mut internal = Vec::with_capacity(resolver.columns.len() * 2);
    for (qualifier, side, relation) in [
        ("old", RuleRowSide::Old, binding.old_relation()),
        ("new", RuleRowSide::New, binding.new_relation()),
    ] {
        if relation.is_none() {
            continue;
        }
        for (name, metadata) in resolver.columns {
            if !required_columns.contains(name) {
                continue;
            }
            let slot = names.len();
            names.push(name.clone());
            identities.push(crate::ColumnIdentity::qualified(qualifier, name));
            types.push(Some(metadata.ty.clone()));
            values.push(resolver.record_field(side, name)?.value);
            let column = match side {
                RuleRowSide::Old => binding.old_column(name),
                RuleRowSide::New => binding.new_column(name),
            };
            if let Some(column) = column {
                internal.push((column, slot, Some(metadata.ty.clone())));
            }
        }
    }
    let schema = crate::RowSchema::with_identities(names, identities, types);
    Ok((
        crate::RowSchema::with_physical_internal_aliases(&schema, &internal),
        crate::PhysicalRow::from_values(values),
    ))
}

pub(super) fn rule_condition_matches<F>(
    context: RuleContext<'_>,
    rule: &uqa_sql::catalog::events::StoredRule,
    privilege_subject: &RoleReference,
    row_index: usize,
    row: &mut RuleRowImage,
    columns: &BTreeMap<String, RuleColumnMetadata>,
    project: &mut F,
) -> Result<bool, SQLError>
where
    F: FnMut(usize, RuleRowSide, &str) -> Result<Option<Value>, SQLError>,
{
    let Some(_) = rule.definition.condition.as_ref() else {
        return Ok(true);
    };
    if let Some((plan, binding)) = rule.bound_condition_plan() {
        let mut required_columns =
            uqa_sql::semantics::rules::action_binding::rule_condition_plan_row_columns(
                plan, binding,
            );
        if uqa_sql::semantics::rules::action_binding::rule_condition_plan_references_whole_row(plan)
        {
            required_columns.extend(columns.keys().cloned());
        }
        let mut resolver = ProjectedRuntimeRuleResolver {
            row_index,
            row,
            columns,
            project,
        };
        let (schema, physical_row) =
            materialize_rule_condition_row(binding, &required_columns, &mut resolver)?;
        return Ok(uqa_sql::expr::truthy(
            &context.expressions.evaluate_stored(
                plan,
                &schema,
                &physical_row,
                privilege_subject,
            )?,
        ));
    }
    Err(SQLError::Internal(
        "stored rule condition has no analyzed plan".into(),
    ))
}
