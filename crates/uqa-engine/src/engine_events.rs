//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable row-trigger and rewrite-rule registries with PostgreSQL-compatible lifecycle.

use serde::{Deserialize, Serialize};

use uqa_sql::ast::{CreateRule, CreateTrigger, EventEnableMode};
use uqa_sql::SQLError;

pub(crate) use rule_binding::{
    bind_rule_action, bind_rule_expr_scoped, expand_rule_action_returning_stars,
    expand_rule_action_row_stars, first_rule_row_reference_in_expr,
    first_rule_row_reference_in_select, rename_rule_action_returning_target_column,
    rename_rule_condition_plan_column, rule_action_has_set_operation,
    rule_action_returning_references_target_column, rule_condition_plan_references_whole_row,
    rule_condition_plan_row_columns, rule_expr_references_row, rule_expr_references_whole_row,
    rule_expr_row_columns, rule_statement_references_row, rule_statement_references_whole_row,
    rule_statement_row_columns,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StoredTrigger {
    pub(crate) definition: CreateTrigger,
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
}

pub(crate) const RULE_OLD_PLAN_QUALIFIER: &str = "\0uqa_rule_old";
pub(crate) const RULE_NEW_PLAN_QUALIFIER: &str = "\0uqa_rule_new";

#[derive(Default, Serialize, Deserialize)]
struct StoredTriggerCatalog {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    triggers: Vec<StoredTrigger>,
}

#[derive(Default, Serialize, Deserialize)]
struct StoredRuleCatalog {
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
mod validation;
