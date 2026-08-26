//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable row-trigger and rewrite-rule registries with PostgreSQL-compatible lifecycle.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use uqa_sql::ast::{CreateRule, CreateTrigger, EventEnableMode, Projection, RuleEvent, Statement};
use uqa_sql::plpgsql::{bind_expr, bind_statement, ResolvedVariable, VariableResolver};
use uqa_sql::SQLError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StoredTrigger {
    pub(crate) definition: CreateTrigger,
    #[serde(default)]
    pub(crate) enabled: EventEnableMode,
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

/// Bind an action's event-row OLD/NEW references while preserving DML
/// RETURNING references that `PostgreSQL` resolves against the action target's
/// own OLD/NEW images first.
pub(crate) fn bind_rule_action(
    action: &Statement,
    event: RuleEvent,
    action_columns: &BTreeSet<String>,
    resolver: &mut dyn VariableResolver,
) -> Result<Statement, SQLError> {
    let returning = match action {
        Statement::Insert(statement) => &statement.returning,
        Statement::Update(statement) => &statement.returning,
        Statement::Delete(statement) => &statement.returning,
        _ => return bind_statement(action, resolver),
    };
    if returning.is_empty() {
        return bind_statement(action, resolver);
    }
    let mut body = action.clone();
    let aliases = match &mut body {
        Statement::Insert(statement) => {
            statement.returning.clear();
            statement.returning_aliases.clone()
        }
        Statement::Update(statement) => {
            statement.returning.clear();
            statement.returning_aliases.clone()
        }
        Statement::Delete(statement) => {
            statement.returning.clear();
            statement.returning_aliases.clone()
        }
        _ => unreachable!("DML rule action changed statement kind"),
    };
    let mut bound = bind_statement(&body, resolver)?;
    let mut returning_resolver = RuleActionReturningResolver {
        event,
        action_columns,
        aliases: &aliases,
        event_resolver: resolver,
    };
    let returning = returning
        .iter()
        .map(|projection| {
            Ok(Projection {
                expr: bind_expr(&projection.expr, &mut returning_resolver)?,
                alias: projection.alias.clone(),
            })
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    match &mut bound {
        Statement::Insert(statement) => statement.returning = returning,
        Statement::Update(statement) => statement.returning = returning,
        Statement::Delete(statement) => statement.returning = returning,
        _ => unreachable!("bound DML rule action changed statement kind"),
    }
    Ok(bound)
}

struct RuleActionReturningResolver<'a> {
    event: RuleEvent,
    action_columns: &'a BTreeSet<String>,
    aliases: &'a uqa_sql::ast::ReturningAliases,
    event_resolver: &'a mut dyn VariableResolver,
}

impl RuleActionReturningResolver<'_> {
    fn action_image_alias(&self, qualifier: &str) -> Option<bool> {
        if qualifier.eq_ignore_ascii_case(&self.aliases.old) {
            return Some(self.aliases.old_explicit);
        }
        if qualifier.eq_ignore_ascii_case(&self.aliases.new) {
            return Some(self.aliases.new_explicit);
        }
        None
    }
}

impl VariableResolver for RuleActionReturningResolver<'_> {
    fn resolve_name(&mut self, name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        self.event_resolver.resolve_name(name)
    }

    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        if let Some(explicit) = self.action_image_alias(qualifier) {
            if self.action_columns.contains(column) {
                return Ok(None);
            }
            if explicit || self.event == RuleEvent::Insert {
                return Err(SQLError::UnknownColumn(format!("{qualifier}.{column}")));
            }
        }
        self.event_resolver.resolve_qualified(qualifier, column)
    }

    fn resolve_param(&mut self, index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        self.event_resolver.resolve_param(index)
    }

    fn rewrite_name(&mut self, name: &str) -> Result<Option<uqa_sql::ast::Expr>, SQLError> {
        self.event_resolver.rewrite_name(name)
    }

    fn rewrite_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<uqa_sql::ast::Expr>, SQLError> {
        if let Some(explicit) = self.action_image_alias(qualifier) {
            if self.action_columns.contains(column) {
                return Ok(None);
            }
            if explicit || self.event == RuleEvent::Insert {
                return Err(SQLError::UnknownColumn(format!("{qualifier}.{column}")));
            }
        }
        self.event_resolver.rewrite_qualified(qualifier, column)
    }

    fn rewrite_param(&mut self, index: usize) -> Result<Option<uqa_sql::ast::Expr>, SQLError> {
        self.event_resolver.rewrite_param(index)
    }
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
mod validation;
