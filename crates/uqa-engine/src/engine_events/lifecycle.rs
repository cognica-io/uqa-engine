//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation and column lifecycle handling for stored triggers.

use std::collections::BTreeMap;

use uqa_sql::ast::{DropRule, DropTrigger, Expr, Statement};
use uqa_sql::plpgsql::{bind_statement, ResolvedVariable, VariableResolver};
use uqa_sql::SQLError;

use crate::{Engine, RelationIdentity, StorageBackendError, StorageBackendResult};

impl Engine {
    pub(crate) fn drop_relation_events_inner(
        &self,
        relation: &RelationIdentity,
    ) -> StorageBackendResult<()> {
        let mut triggers = self.durable.triggers.write();
        let mut rules = self.durable.rules.write();
        let qualified = relation.qualified_name();
        let referenced_by_trigger = triggers.values().any(|entries| {
            entries.values().any(|trigger| {
                trigger.definition.referenced_table.as_deref() == Some(qualified.as_str())
            })
        });
        if !triggers.contains_key(relation)
            && !rules.contains_key(relation)
            && !referenced_by_trigger
        {
            return Ok(());
        }
        let mut next_triggers = triggers.clone();
        let mut next_rules = rules.clone();
        let mut removed_constraint_identities = Vec::new();
        for (trigger_relation, entries) in triggers.iter() {
            for trigger in entries.values() {
                if trigger.definition.constraint
                    && (trigger_relation == relation
                        || trigger.definition.referenced_table.as_deref()
                            == Some(qualified.as_str()))
                {
                    removed_constraint_identities.push(
                        Self::constraint_trigger_identity(trigger).map_err(|error| {
                            StorageBackendError::Other(format!(
                                "resolve dropped constraint-trigger identity: {error}"
                            ))
                        })?,
                    );
                }
            }
        }
        next_triggers.remove(relation);
        for entries in next_triggers.values_mut() {
            entries.retain(|_, trigger| {
                trigger.definition.referenced_table.as_deref() != Some(qualified.as_str())
            });
        }
        next_triggers.retain(|_, entries| !entries.is_empty());
        next_rules.remove(relation);
        self.persist_trigger_catalog_snapshot(&next_triggers)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        self.persist_rule_catalog_snapshot(&next_rules)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        *triggers = next_triggers;
        *rules = next_rules;
        drop(rules);
        drop(triggers);
        for identity in &removed_constraint_identities {
            self.forget_constraint_trigger_events(identity);
        }
        self.note_catalog_registry_changed();
        Ok(())
    }

    pub(crate) fn rename_relation_events_inner(
        &self,
        from: &RelationIdentity,
        to: &RelationIdentity,
    ) -> StorageBackendResult<()> {
        let mut triggers = self.durable.triggers.write();
        let mut rules = self.durable.rules.write();
        let from_name = from.qualified_name();
        let to_name = to.qualified_name();
        let referenced_by_trigger = triggers.values().any(|entries| {
            entries.values().any(|trigger| {
                trigger.definition.referenced_table.as_deref() == Some(from_name.as_str())
            })
        });
        if !triggers.contains_key(from) && !rules.contains_key(from) && !referenced_by_trigger {
            return Ok(());
        }
        let mut next_triggers = triggers.clone();
        let mut next_rules = rules.clone();
        if let Some(mut entries) = next_triggers.remove(from) {
            for trigger in entries.values_mut() {
                trigger.definition.table.clone_from(&to_name);
            }
            next_triggers.insert(to.clone(), entries);
        }
        for entries in next_triggers.values_mut() {
            for trigger in entries.values_mut() {
                if trigger.definition.referenced_table.as_deref() == Some(from_name.as_str()) {
                    trigger.definition.referenced_table = Some(to_name.clone());
                }
            }
        }
        if let Some(mut entries) = next_rules.remove(from) {
            for rule in entries.values_mut() {
                rule.definition.table = to.qualified_name();
            }
            next_rules.insert(to.clone(), entries);
        }
        self.persist_trigger_catalog_snapshot(&next_triggers)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        self.persist_rule_catalog_snapshot(&next_rules)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        *triggers = next_triggers;
        *rules = next_rules;
        drop(rules);
        drop(triggers);
        self.note_catalog_registry_changed();
        Ok(())
    }

    pub(crate) fn rename_event_column_inner(
        &self,
        table: &str,
        from: &str,
        to: &str,
    ) -> StorageBackendResult<()> {
        let relation =
            RelationIdentity::from_legacy_name(table).map_err(StorageBackendError::Other)?;
        let mut triggers = self.durable.triggers.write();
        let mut rules = self.durable.rules.write();
        if !triggers.contains_key(&relation) && !rules.contains_key(&relation) {
            return Ok(());
        }
        let mut next_triggers = triggers.clone();
        let mut next_rules = rules.clone();
        if let Some(entries) = next_triggers.get_mut(&relation) {
            for trigger in entries.values_mut() {
                for column in &mut trigger.definition.update_columns {
                    if column == from {
                        *column = to.to_string();
                    }
                }
                if let Some(condition) = trigger.definition.when.as_mut() {
                    crate::engine_table_storage::rename_schema_expr_column(condition, from, to)?;
                }
            }
        }
        if let Some(entries) = next_rules.get_mut(&relation) {
            for rule in entries.values_mut() {
                if let Some(condition) = rule.definition.condition.as_mut() {
                    crate::engine_table_storage::rename_schema_expr_column(condition, from, to)?;
                }
                for action in &mut rule.definition.actions {
                    *action = rename_rule_statement_column(action, from, to).map_err(|error| {
                        StorageBackendError::Other(format!("rename rule column: {error}"))
                    })?;
                }
                for action_sql in &mut rule.definition.action_sql {
                    *action_sql = rename_rule_action_sql_column(action_sql, from, to);
                }
            }
        }
        self.persist_trigger_catalog_snapshot(&next_triggers)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        self.persist_rule_catalog_snapshot(&next_rules)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        *triggers = next_triggers;
        *rules = next_rules;
        drop(rules);
        drop(triggers);
        self.note_catalog_registry_changed();
        Ok(())
    }

    pub(crate) fn handle_drop_column_event_dependencies(
        &self,
        table: &str,
        column: &str,
        cascade: bool,
    ) -> Result<(), SQLError> {
        let relation = RelationIdentity::from_legacy_name(table).map_err(|error| {
            SQLError::Internal(format!("decode trigger relation `{table}`: {error}"))
        })?;
        let dependent_triggers = self
            .durable
            .triggers
            .read()
            .get(&relation)
            .into_iter()
            .flat_map(BTreeMap::values)
            .filter(|trigger| {
                trigger
                    .definition
                    .update_columns
                    .iter()
                    .any(|name| name == column)
                    || trigger.definition.when.as_ref().is_some_and(|condition| {
                        crate::engine_table_storage::schema_expr_references_column(
                            condition, column,
                        )
                    })
            })
            .map(|trigger| trigger.definition.name.clone())
            .collect::<Vec<_>>();
        let dependent_rules = self
            .durable
            .rules
            .read()
            .get(&relation)
            .into_iter()
            .flat_map(BTreeMap::values)
            .filter(|rule| {
                rule.definition.condition.as_ref().is_some_and(|condition| {
                    crate::engine_table_storage::schema_expr_references_column(condition, column)
                }) || rule
                    .definition
                    .actions
                    .iter()
                    .any(|action| rule_statement_references_column(action, column))
            })
            .map(|rule| rule.definition.name.clone())
            .collect::<Vec<_>>();
        if dependent_triggers.is_empty() && dependent_rules.is_empty() {
            return Ok(());
        }
        if !cascade {
            let mut objects = dependent_triggers
                .iter()
                .map(|name| format!("trigger {name}"))
                .collect::<Vec<_>>();
            objects.extend(dependent_rules.iter().map(|name| format!("rule {name}")));
            return Err(SQLError::Routine {
                sqlstate: "2BP01".into(),
                message: format!(
                    "cannot drop column {column} of table {table} because {} depends on it",
                    objects.join(", ")
                ),
            });
        }
        for name in dependent_triggers {
            self.drop_trigger(&DropTrigger {
                name: name.clone(),
                table: table.to_string(),
                if_exists: false,
                cascade: true,
            })?;
            self.push_sql_notice(
                "NOTICE",
                &format!("drop cascades to trigger {name} on table {table}"),
            );
        }
        for name in dependent_rules {
            self.drop_rule(&DropRule {
                name: name.clone(),
                table: table.to_string(),
                if_exists: false,
                cascade: true,
            })?;
            self.push_sql_notice(
                "NOTICE",
                &format!("drop cascades to rule {name} on table {table}"),
            );
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

fn rename_rule_statement_column(
    statement: &Statement,
    from: &str,
    to: &str,
) -> Result<Statement, SQLError> {
    bind_statement(
        statement,
        &mut RuleColumnResolver {
            from,
            to: Some(to),
            referenced: false,
        },
    )
}

fn rule_statement_references_column(statement: &Statement, column: &str) -> bool {
    let mut resolver = RuleColumnResolver {
        from: column,
        to: None,
        referenced: false,
    };
    let _ = bind_statement(statement, &mut resolver);
    resolver.referenced
}

fn rename_rule_action_sql_column(sql: &str, from: &str, to: &str) -> String {
    let mut rewritten = String::with_capacity(sql.len());
    let mut copied_through = 0;
    let mut cursor = 0;
    while cursor < sql.len() {
        if sql.as_bytes()[cursor] == b'\'' {
            cursor = sql_string_end(sql, cursor);
            continue;
        }
        let Some((qualifier_end, qualifier)) = sql_identifier(sql, cursor) else {
            cursor += sql[cursor..].chars().next().map_or(1, char::len_utf8);
            continue;
        };
        if !qualifier.eq_ignore_ascii_case("old") && !qualifier.eq_ignore_ascii_case("new") {
            cursor = qualifier_end;
            continue;
        }
        let dot = skip_sql_whitespace(sql, qualifier_end);
        if sql.as_bytes().get(dot) != Some(&b'.') {
            cursor = qualifier_end;
            continue;
        }
        let column_start = skip_sql_whitespace(sql, dot + 1);
        let Some((column_end, column)) = sql_identifier(sql, column_start) else {
            cursor = qualifier_end;
            continue;
        };
        if column != from {
            cursor = column_end;
            continue;
        }
        rewritten.push_str(&sql[copied_through..column_start]);
        rewritten.push_str(&uqa_sql::expr::quote_ident(to));
        copied_through = column_end;
        cursor = column_end;
    }
    rewritten.push_str(&sql[copied_through..]);
    rewritten
}

fn sql_identifier(sql: &str, start: usize) -> Option<(usize, String)> {
    let bytes = sql.as_bytes();
    let first = *bytes.get(start)?;
    if first == b'"' {
        let mut value = String::new();
        let mut cursor = start + 1;
        while cursor < bytes.len() {
            if bytes[cursor] == b'"' {
                if bytes.get(cursor + 1) == Some(&b'"') {
                    value.push('"');
                    cursor += 2;
                    continue;
                }
                return Some((cursor + 1, value));
            }
            let character = sql[cursor..].chars().next()?;
            value.push(character);
            cursor += character.len_utf8();
        }
        return None;
    }
    if !sql_identifier_start(first) {
        return None;
    }
    let mut cursor = start + 1;
    while bytes
        .get(cursor)
        .is_some_and(|byte| sql_identifier_continue(*byte))
    {
        cursor += 1;
    }
    Some((cursor, sql[start..cursor].to_string()))
}

const fn sql_identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte >= 0x80
}

const fn sql_identifier_continue(byte: u8) -> bool {
    sql_identifier_start(byte) || byte.is_ascii_digit() || byte == b'$'
}

fn skip_sql_whitespace(sql: &str, mut cursor: usize) -> usize {
    while sql
        .as_bytes()
        .get(cursor)
        .is_some_and(u8::is_ascii_whitespace)
    {
        cursor += 1;
    }
    cursor
}

fn sql_string_end(sql: &str, start: usize) -> usize {
    let bytes = sql.as_bytes();
    let mut cursor = start + 1;
    while cursor < bytes.len() {
        if bytes[cursor] == b'\\' {
            cursor = (cursor + 2).min(bytes.len());
        } else if bytes[cursor] == b'\'' {
            if bytes.get(cursor + 1) == Some(&b'\'') {
                cursor += 2;
            } else {
                return cursor + 1;
            }
        } else {
            cursor += 1;
        }
    }
    bytes.len()
}
