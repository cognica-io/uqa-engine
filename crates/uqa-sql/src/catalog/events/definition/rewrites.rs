//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rebind event definitions and stored dependencies after routine and column changes.
use super::EventAnalysisContext;
use crate::{
    ast::{Expr, FunctionBinding},
    catalog::{
        events::{
            PreparedRuleColumnDrop, RuleCatalog, RuleColumnDependency, StoredRule, TriggerCatalog,
        },
        resolution::RelationLookupMode,
    },
    plpgsql::{ResolvedVariable, VariableResolver},
    SQLError,
};
use std::collections::BTreeMap;
use uqa_core::RelationIdentity;
pub fn rewrite_trigger_routine_references(
    next_triggers: &mut TriggerCatalog,
    target: &FunctionBinding,
    new_name: &str,
) -> Result<bool, SQLError> {
    let mut triggers_changed = false;
    for trigger in next_triggers.values_mut().flat_map(BTreeMap::values_mut) {
        let invokes_target = match (trigger.function_object_id, target.object_id) {
            (Some(stored), Some(target)) => stored == target,
            (None, None) => {
                target.argument_types.is_empty() && trigger.definition.function == target.name
            }
            _ => false,
        };
        if invokes_target {
            trigger.definition.function = new_name.to_string();
            triggers_changed = true;
        }
        if let Some(condition) = &mut trigger.definition.when {
            triggers_changed |= crate::catalog::stored_ast::rewrite_expression_routine_identity(
                condition, target, new_name,
            )?;
        }
    }

    Ok(triggers_changed)
}
impl EventAnalysisContext<'_> {
    pub fn rewrite_rule_routine_references(
        &self,
        next_rules: &mut RuleCatalog,
        target: &FunctionBinding,
        new_name: &str,
    ) -> Result<bool, SQLError> {
        let mut rules_changed = false;
        for (event_relation, entries) in next_rules {
            for rule in entries.values_mut() {
                let references_target = rule
                    .dependencies
                    .as_ref()
                    .ok_or_else(|| {
                        SQLError::Internal(format!(
                            "rule `{}` on `{}` has no bound dependency state",
                            rule.definition.name,
                            event_relation.qualified_name()
                        ))
                    })?
                    .routines
                    .iter()
                    .any(
                        |dependency| match (dependency.object_id, target.object_id) {
                            (Some(dependency), Some(target)) => dependency == target,
                            (None, None) => {
                                dependency.name == target.name
                                    && dependency.argument_types == target.argument_types
                            }
                            _ => false,
                        },
                    );
                if !references_target {
                    continue;
                }
                if let Some(condition) = &mut rule.definition.condition {
                    crate::catalog::stored_ast::rewrite_expression_routine_identity(
                        condition, target, new_name,
                    )?;
                }
                for action in &mut rule.definition.actions {
                    crate::catalog::stored_ast::rewrite_statement_routine_identity(
                        action, target, new_name,
                    )?;
                }
                crate::catalog::events::synchronize_rule_sql_text(&mut rule.definition)?;
                let (validated_relation, condition_plan, condition_binding, dependencies) = self
                    .validate_rule_definition(
                        &mut rule.definition,
                        RelationLookupMode::Bound,
                        None,
                        None,
                    )?;
                if &validated_relation != event_relation {
                    return Err(SQLError::Internal(format!(
                        "rewritten rule `{}` moved from `{}` to `{}`",
                        rule.definition.name,
                        event_relation.qualified_name(),
                        validated_relation.qualified_name()
                    )));
                }
                rule.condition_plan = condition_plan;
                rule.condition_binding = condition_binding;
                rule.dependencies = Some(dependencies);
                rules_changed = true;
            }
        }

        Ok(rules_changed)
    }
    pub fn renamed_event_column(
        &self,
        triggers: &TriggerCatalog,
        rules: &RuleCatalog,
        relation: &RelationIdentity,
        from: &str,
        to: &str,
    ) -> Result<Option<(TriggerCatalog, RuleCatalog)>, String> {
        let dependency = RuleColumnDependency {
            relation: relation.clone(),
            column: from.to_string(),
        };
        let referenced_by_rule = rules.values().any(|entries| {
            entries.values().any(|rule| {
                rule.dependencies
                    .as_ref()
                    .is_some_and(|dependencies| dependencies.columns.contains(&dependency))
            })
        });
        if !triggers.contains_key(relation) && !rules.contains_key(relation) && !referenced_by_rule
        {
            return Ok(None);
        }
        let mut next_triggers = triggers.clone();
        let mut next_rules = rules.clone();
        if let Some(entries) = next_triggers.get_mut(relation) {
            for trigger in entries.values_mut() {
                for column in &mut trigger.definition.update_columns {
                    if column == from {
                        *column = to.to_string();
                    }
                }
                if let Some(condition) = trigger.definition.when.as_mut() {
                    crate::schema::dependencies::rewrites::rename_schema_expr_column(
                        condition, from, to,
                    )?;
                }
            }
        }
        self.rewrite_rule_catalog_column(&mut next_rules, &dependency, relation, from, to)?;

        Ok(Some((next_triggers, next_rules)))
    }
    pub fn rewrite_rule_catalog_column(
        &self,
        rules: &mut BTreeMap<RelationIdentity, BTreeMap<String, StoredRule>>,
        dependency: &RuleColumnDependency,
        relation: &RelationIdentity,
        from: &str,
        to: &str,
    ) -> Result<(), String> {
        for (event_relation, entries) in rules {
            for rule in entries.values_mut() {
                let dependencies = rule.dependencies.as_ref().ok_or_else(|| {
                    format!(
                        "rule `{}` on `{}` has no bound dependency state",
                        rule.definition.name,
                        event_relation.qualified_name()
                    )
                })?;
                if dependencies.columns.contains(dependency) {
                    self.rewrite_stored_rule_column(rule, event_relation, relation, from, to)?;
                }
            }
        }
        Ok(())
    }

    fn rewrite_stored_rule_column(
        &self,
        rule: &mut StoredRule,
        event_relation: &RelationIdentity,
        relation: &RelationIdentity,
        from: &str,
        to: &str,
    ) -> Result<(), String> {
        if event_relation == relation {
            self.rewrite_rule_event_row_column(rule, from, to)?;
        }
        crate::binding::stored_columns::rewrite_rule_column_references(
            self.columns,
            &mut rule.definition,
            relation,
            from,
            to,
        )
        .map_err(|error| {
            format!(
                "rewrite rule `{}` column dependency: {error}",
                rule.definition.name
            )
        })?;
        let (validated_relation, condition_plan, condition_binding, dependencies) = self
            .validate_rule_definition(&mut rule.definition, RelationLookupMode::Bound, None, None)
            .map_err(|error| {
                format!(
                    "rebind rule `{}` after column rename: {error}",
                    rule.definition.name
                )
            })?;
        if validated_relation != *event_relation {
            return Err(format!(
                "rule `{}` changed event relation while rebinding column rename",
                rule.definition.name
            ));
        }
        rule.condition_plan = condition_plan;
        rule.condition_binding = condition_binding;
        rule.dependencies = Some(dependencies);
        Ok(())
    }

    fn rewrite_rule_event_row_column(
        &self,
        rule: &mut StoredRule,
        from: &str,
        to: &str,
    ) -> Result<(), String> {
        if let Some(condition) = rule.definition.condition.as_mut() {
            *condition = crate::semantics::rules::action_binding::bind_rule_expr_scoped(
                condition,
                &mut RuleColumnResolver {
                    from,
                    to: Some(to),
                    referenced: false,
                },
                &std::collections::BTreeSet::new(),
            )
            .map_err(|error| format!("rename rule condition column: {error}"))?;
        }
        for action in &mut rule.definition.actions {
            let action_columns = self
                .rule_action_target_columns(action)
                .map_err(|error| format!("read rule action columns during rename: {error}"))?;
            *action = crate::semantics::rules::action_binding::bind_rule_action(
                self.sources,
                action,
                &action_columns,
                &mut RuleColumnResolver {
                    from,
                    to: Some(to),
                    referenced: false,
                },
            )
            .map_err(|error| format!("rename rule event column: {error}"))?;
        }
        Ok(())
    }
    pub fn rebind_rule_column_drop(
        &self,
        prepared: &mut PreparedRuleColumnDrop,
    ) -> Result<(), String> {
        for (event_relation, name) in &prepared.rebind {
            let rule = prepared
                .rules
                .get_mut(event_relation)
                .and_then(|entries| entries.get_mut(name))
                .ok_or_else(|| {
                    format!(
                        "rule `{name}` on `{}` disappeared while dropping a column",
                        event_relation.qualified_name()
                    )
                })?;
            let (validated_relation, condition_plan, condition_binding, dependencies) = self
                .validate_rule_definition(
                    &mut rule.definition,
                    RelationLookupMode::Bound,
                    None,
                    None,
                )
                .map_err(|error| {
                    format!(
                        "rebind rule `{}` after column drop: {error}",
                        rule.definition.name
                    )
                })?;
            if validated_relation != *event_relation {
                return Err(format!(
                    "rule `{}` changed event relation while rebinding column drop",
                    rule.definition.name
                ));
            }
            rule.condition_plan = condition_plan;
            rule.condition_binding = condition_binding;
            rule.dependencies = Some(dependencies);
        }

        Ok(())
    }
}

struct RuleColumnResolver<'a> {
    from: &'a str,
    to: Option<&'a str>,
    referenced: bool,
}

impl VariableResolver for RuleColumnResolver<'_> {
    fn resolve_name(&mut self, _name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn resolve_qualified(
        &mut self,
        _qualifier: &str,
        _column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn resolve_param(&mut self, _index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn rewrite_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<Expr>, SQLError> {
        if (qualifier.eq_ignore_ascii_case("old") || qualifier.eq_ignore_ascii_case("new"))
            && column == self.from
        {
            self.referenced = true;
            if let Some(to) = self.to {
                return Ok(Some(Expr::qualified_column(qualifier, to)));
            }
        }
        Ok(None)
    }
}
