//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable row-trigger and rewrite-rule registries with PostgreSQL-compatible lifecycle.

use serde::{Deserialize, Serialize};

use uqa_sql::SQLError;

pub(crate) use rule_binding::{
    bind_rule_action, bind_rule_expr_scoped, expand_rule_action_returning_stars,
    expand_rule_action_row_stars, first_rule_row_reference_in_expr,
    first_rule_row_reference_in_select, rule_action_has_set_operation, rule_expr_row_columns,
    rule_new_row_columns, rule_statement_row_columns,
};
pub(crate) use rule_condition_binding::RuleConditionBinding;
pub(crate) use rule_dependencies::{
    bind_stored_expression_routines, bind_stored_statement_routines,
    copy_stored_source_column_shapes, expression_references_routine_identity,
    rewrite_expression_routine_identity, rewrite_statement_routine_identity,
    rewrite_stored_statement_relation, visit_stored_expression, visit_stored_statement_expressions,
    visit_stored_statement_merges, visit_stored_statement_sources,
};

const RULE_CATALOG_FORMAT_VERSION: u32 = 3;

pub(crate) use uqa_sql::catalog::events::{
    RuleColumnDependency, RuleDependencies, RuleRoutineDependency, StoredRule, StoredTrigger,
};

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
