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
    bind_rule_action, bind_rule_expr_scoped, first_rule_row_reference_in_expr,
    first_rule_row_reference_in_select, rule_action_has_set_operation, rule_expr_references_row,
    rule_statement_references_row,
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
}

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

mod lifecycle;
mod lookup;
mod persistence;
mod registry;
mod rule_binding;
mod validation;
