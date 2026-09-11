//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable row-trigger and rewrite-rule registries with PostgreSQL-compatible lifecycle.

use serde::{Deserialize, Serialize};

pub(crate) use uqa_sql::catalog::stored_ast::{
    expression_references_routine_identity, rewrite_expression_routine_identity,
    rewrite_statement_routine_identity,
};
pub(crate) use uqa_sql::semantics::rules::action_binding::{
    bind_rule_action, bind_rule_expr_scoped, rule_new_row_columns,
};

const RULE_CATALOG_FORMAT_VERSION: u32 = 3;

pub(crate) use uqa_sql::catalog::events::{RuleColumnDependency, StoredRule, StoredTrigger};

pub(crate) use uqa_sql::catalog::events::PreparedRuleColumnDrop;

use uqa_sql::catalog::events::synchronize_rule_sql_text;

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

mod lifecycle;
mod lookup;
mod persistence;

#[cfg(test)]
mod tests;
