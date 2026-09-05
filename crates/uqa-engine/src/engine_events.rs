//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable row-trigger and rewrite-rule registries with PostgreSQL-compatible lifecycle.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

use uqa_sql::ast::{CreateRule, CreateTrigger, EventEnableMode};
use uqa_sql::SQLError;

pub(crate) use rule_binding::{
    bind_rule_action, bind_rule_expr_scoped, expand_rule_action_returning_stars,
    expand_rule_action_row_stars, first_rule_row_reference_in_expr,
    first_rule_row_reference_in_select, rule_action_has_set_operation,
    rule_condition_plan_references_whole_row, rule_condition_plan_row_columns,
    rule_expr_references_row, rule_expr_references_whole_row, rule_expr_row_columns,
    rule_statement_references_row, rule_statement_references_whole_row, rule_statement_row_columns,
};
pub(crate) use rule_condition_binding::RuleConditionBinding;
pub(crate) use rule_dependencies::{
    bind_stored_expression_routines, bind_stored_statement_routines,
    expression_references_routine_identity, rewrite_expression_routine_identity,
    rewrite_statement_routine_identity, statement_references_routine_identity,
};

const RULE_CATALOG_FORMAT_VERSION: u32 = 3;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RuleDependencies {
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub(crate) relations: BTreeSet<crate::RelationIdentity>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub(crate) columns: BTreeSet<RuleColumnDependency>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub(crate) routines: BTreeSet<RuleRoutineDependency>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct RuleColumnDependency {
    pub(crate) relation: crate::RelationIdentity,
    pub(crate) column: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct RuleRoutineDependency {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) object_id: Option<[u8; 16]>,
    pub(crate) name: String,
    pub(crate) argument_types: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StoredTrigger {
    pub(crate) definition: CreateTrigger,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) function_object_id: Option<[u8; 16]>,
    #[serde(default)]
    pub(crate) enabled: EventEnableMode,
    #[serde(default)]
    pub(crate) object_id: Option<[u8; 16]>,
    #[serde(default)]
    pub(crate) constraint_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StoredRule {
    pub(crate) definition: CreateRule,
    #[serde(default)]
    pub(crate) enabled: EventEnableMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) condition_plan: Option<uqa_planner::ExpressionPlan>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) condition_binding: Option<RuleConditionBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) dependencies: Option<RuleDependencies>,
}

pub(crate) struct PreparedRuleColumnDrop {
    rules: BTreeMap<crate::RelationIdentity, BTreeMap<String, StoredRule>>,
    rebind: BTreeSet<(crate::RelationIdentity, String)>,
}

fn synchronize_rule_sql_text(definition: &mut CreateRule) -> Result<(), SQLError> {
    definition.condition_sql = definition
        .condition
        .as_ref()
        .map(uqa_sql::render::expression_sql)
        .transpose()?;
    definition.action_sql = definition
        .actions
        .iter()
        .map(uqa_sql::render::statement_sql)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(())
}

impl StoredRule {
    pub(crate) fn bound_condition_plan(
        &self,
    ) -> Option<(&uqa_planner::ExpressionPlan, &RuleConditionBinding)> {
        self.condition_plan
            .as_ref()
            .zip(self.condition_binding.as_ref())
    }
}

#[derive(Default, Serialize, Deserialize)]
struct StoredTriggerCatalog {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    triggers: Vec<StoredTrigger>,
}

#[derive(Default, Serialize, Deserialize)]
struct StoredRuleCatalog {
    #[serde(default)]
    format_version: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    rules: Vec<StoredRule>,
}

fn duplicate_object(kind: &str, name: &str, table: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42710".into(),
        message: format!("{kind} \"{name}\" for relation \"{table}\" already exists"),
    }
}

fn undefined_object(kind: &str, name: &str, table: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42704".into(),
        message: format!("{kind} \"{name}\" for table \"{table}\" does not exist"),
    }
}

fn undefined_rule(name: &str, relation: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42704".into(),
        message: format!("rule \"{name}\" for relation \"{relation}\" does not exist"),
    }
}

mod lifecycle;
mod lookup;
mod persistence;
mod registry;
mod rule_binding;
mod rule_columns;
mod rule_condition_binding;
mod rule_dependencies;
mod validation;
