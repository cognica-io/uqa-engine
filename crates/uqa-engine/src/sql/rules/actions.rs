//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Set-oriented rewrite-rule action binding and internal OLD/NEW row sources.

use std::collections::{BTreeMap, BTreeSet};

use uqa_core::Value;
use uqa_sql::ast::{Expr, FromClause, JoinKind, SelectStmt, Statement};
use uqa_sql::plpgsql::{
    bind_expr, bind_select, bind_statement, ResolvedVariable, VariableResolver,
};
use uqa_sql::SQLError;

use crate::Engine;

use super::{RuleColumnMetadata, RuleRowImage, RuntimeRuleResolver};

pub(super) fn bind_insert_values_action(
    engine: &Engine,
    action: &Statement,
    matching_rows: &[usize],
    rows: &[RuleRowImage],
    columns: &BTreeMap<String, RuleColumnMetadata>,
) -> Result<Statement, SQLError> {
    let action_columns = super::rule_action_target_columns(engine, action)?;
    let mut combined = None;
    let mut combined_contract = None;
    for row_index in matching_rows {
        let row = rows.get(*row_index).ok_or_else(|| {
            SQLError::Internal("rewrite rule lost its qualified row image".into())
        })?;
        let bound = crate::engine_events::bind_rule_action(
            action,
            &action_columns,
            &mut runtime_rule_resolver(row, columns),
        )?;
        let Statement::Insert(mut insert) = bound else {
            return Err(SQLError::Internal(
                "rewrite rule INSERT VALUES action changed statement kind".into(),
            ));
        };
        let contract = bound_insert_contract(&insert)?;
        if let Some(expected) = combined_contract.as_ref() {
            if expected != &contract {
                return Err(SQLError::Internal(
                    "rewrite rule INSERT VALUES action produced row-dependent statement clauses"
                        .into(),
                ));
            }
        } else {
            combined_contract = Some(contract);
        }
        if let Some(Statement::Insert(existing)) = combined.as_mut() {
            existing.rows.append(&mut insert.rows);
        } else {
            combined = Some(Statement::Insert(insert));
        }
    }
    combined.ok_or_else(|| SQLError::Internal("rewrite rule action lost its row source".into()))
}

fn bound_insert_contract(insert: &uqa_sql::ast::InsertStmt) -> Result<Vec<u8>, SQLError> {
    let mut contract = insert.clone();
    contract.rows.clear();
    serde_json::to_vec(&contract)
        .map_err(|error| SQLError::Internal(format!("encode rewrite rule INSERT action: {error}")))
}

fn runtime_rule_resolver<'a>(
    row: &'a RuleRowImage,
    columns: &'a BTreeMap<String, RuleColumnMetadata>,
) -> RuntimeRuleResolver<'a> {
    RuntimeRuleResolver {
        old: row.old.as_ref(),
        new: row.new.as_ref(),
        old_doc_id: row.old_doc_id,
        new_doc_id: row.new_doc_id,
        columns,
    }
}

struct RuleRowSource {
    clause: FromClause,
    qualifier: String,
    old_columns: BTreeMap<String, String>,
    new_columns: BTreeMap<String, String>,
    row_index_column: String,
}

pub(super) struct BoundSetOrientedAction {
    pub(super) statement: Statement,
    pub(super) source_index: Expr,
}

pub(super) fn bind_set_oriented_action(
    engine: &Engine,
    action: &Statement,
    matching_rows: &[usize],
    rows: &[RuleRowImage],
    columns: &BTreeMap<String, RuleColumnMetadata>,
    qualifier: &str,
) -> Result<BoundSetOrientedAction, SQLError> {
    let qualifier = unique_rule_source_qualifier(action, qualifier);
    let source = rule_row_source(matching_rows, rows, columns, &qualifier)?;
    let source_index = Expr::qualified_column(&source.qualifier, &source.row_index_column);
    let action_columns = super::rule_action_target_columns(engine, action)?;
    let mut bound = crate::engine_events::bind_rule_action(
        action,
        &action_columns,
        &mut RuleSourceResolver {
            qualifier: &source.qualifier,
            old_columns: &source.old_columns,
            new_columns: &source.new_columns,
        },
    )?;
    attach_rule_row_source(&mut bound, source.clause, &source.qualifier)?;
    Ok(BoundSetOrientedAction {
        statement: bound,
        source_index,
    })
}

fn rule_row_source(
    matching_rows: &[usize],
    rows: &[RuleRowImage],
    columns: &BTreeMap<String, RuleColumnMetadata>,
    qualifier: &str,
) -> Result<RuleRowSource, SQLError> {
    let row_index_column = "__row_index".to_string();
    let mut column_aliases = Vec::with_capacity(columns.len() * 2 + 1);
    let mut old_columns = BTreeMap::new();
    let mut new_columns = BTreeMap::new();
    for (index, column) in columns.keys().enumerate() {
        let old_alias = format!("__old_{index}");
        let new_alias = format!("__new_{index}");
        column_aliases.push(old_alias.clone());
        column_aliases.push(new_alias.clone());
        old_columns.insert(column.clone(), old_alias);
        new_columns.insert(column.clone(), new_alias);
    }
    column_aliases.push(row_index_column.clone());
    let values = matching_rows
        .iter()
        .map(|row_index| {
            let row = rows.get(*row_index).ok_or_else(|| {
                SQLError::Internal("rewrite rule lost its qualified row image".into())
            })?;
            let resolver = runtime_rule_resolver(row, columns);
            let mut values = Vec::with_capacity(column_aliases.len());
            for column in columns.keys() {
                values.push(resolved_variable_expr(resolver.record_field(
                    row.old.as_ref(),
                    row.old_doc_id,
                    column,
                )?));
                values.push(resolved_variable_expr(resolver.record_field(
                    row.new.as_ref(),
                    row.new_doc_id,
                    column,
                )?));
            }
            let row_index = i64::try_from(*row_index).map_err(|_| {
                SQLError::Internal("rewrite rule event row index exceeds BIGINT".into())
            })?;
            values.push(Expr::Literal(Value::Int(row_index)));
            Ok(values)
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    Ok(RuleRowSource {
        clause: FromClause::Values {
            rows: values,
            alias: Some(qualifier.to_string()),
            column_aliases,
        },
        qualifier: qualifier.to_string(),
        old_columns,
        new_columns,
        row_index_column,
    })
}

fn resolved_variable_expr(variable: ResolvedVariable) -> Expr {
    let ResolvedVariable {
        value,
        declared_type,
    } = variable;
    match declared_type {
        Some(ty) => Expr::Cast {
            expr: Box::new(Expr::Literal(value)),
            ty,
        },
        None => Expr::Literal(value),
    }
}

struct RuleSourceResolver<'a> {
    qualifier: &'a str,
    old_columns: &'a BTreeMap<String, String>,
    new_columns: &'a BTreeMap<String, String>,
}

impl VariableResolver for RuleSourceResolver<'_> {
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
        let columns = if qualifier.eq_ignore_ascii_case("old") {
            self.old_columns
        } else if qualifier.eq_ignore_ascii_case("new") {
            self.new_columns
        } else {
            return Ok(None);
        };
        let source_column = columns
            .get(column)
            .ok_or_else(|| SQLError::UnknownColumn(format!("{qualifier}.{column}")))?;
        Ok(Some(Expr::qualified_column(self.qualifier, source_column)))
    }
}

fn attach_rule_row_source(
    statement: &mut Statement,
    source: FromClause,
    qualifier: &str,
) -> Result<(), SQLError> {
    match statement {
        Statement::Select(select) => attach_select_rule_source(select, &source, qualifier),
        Statement::Insert(insert) => {
            let select = insert.select_source.as_mut().ok_or_else(|| {
                SQLError::Internal("set-oriented rule INSERT action has no SELECT source".into())
            })?;
            attach_select_rule_source(select, &source, qualifier);
        }
        Statement::Update(update) => {
            update.from = Some(prepend_rule_row_source(
                update.from.take(),
                source,
                qualifier,
            ));
        }
        Statement::Delete(delete) => {
            delete.using = Some(prepend_rule_row_source(
                delete.using.take(),
                source,
                qualifier,
            ));
        }
        _ => {
            return Err(SQLError::Internal(
                "validated rewrite-rule action changed statement kind".into(),
            ))
        }
    }
    Ok(())
}

fn attach_select_rule_source(select: &mut SelectStmt, source: &FromClause, qualifier: &str) {
    if let Some(set_op) = select.set_op.as_mut() {
        if let Some(left) = set_op.left.as_mut() {
            attach_select_rule_source(left, source, qualifier);
        } else {
            select.from = Some(prepend_rule_row_source(
                select.from.take(),
                source.clone(),
                qualifier,
            ));
        }
        attach_select_rule_source(&mut set_op.right, source, qualifier);
    } else {
        select.from = Some(prepend_rule_row_source(
            select.from.take(),
            source.clone(),
            qualifier,
        ));
    }
}

fn prepend_rule_row_source(
    existing: Option<FromClause>,
    source: FromClause,
    qualifier: &str,
) -> FromClause {
    let Some(existing) = existing else {
        return source;
    };
    let lateral = from_references_qualifier(&existing, qualifier);
    FromClause::Join {
        left: Box::new(source),
        right: Box::new(existing),
        kind: JoinKind::Cross,
        on: None,
        using: None,
        natural: false,
        alias: None,
        column_aliases: Vec::new(),
        lateral,
    }
}

struct QualifierReferenceResolver<'a> {
    qualifier: &'a str,
    referenced: bool,
}

impl VariableResolver for QualifierReferenceResolver<'_> {
    fn resolve_name(&mut self, _name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        _column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        if qualifier.eq_ignore_ascii_case(self.qualifier) {
            self.referenced = true;
        }
        Ok(None)
    }

    fn resolve_param(&mut self, _index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }
}

fn expr_references_qualifier(expr: &Expr, qualifier: &str) -> bool {
    let mut resolver = QualifierReferenceResolver {
        qualifier,
        referenced: false,
    };
    let _ = bind_expr(expr, &mut resolver);
    resolver.referenced
}

fn select_references_qualifier(select: &SelectStmt, qualifier: &str) -> bool {
    let mut resolver = QualifierReferenceResolver {
        qualifier,
        referenced: false,
    };
    let _ = bind_select(select, &mut resolver);
    resolver.referenced
}

fn from_references_qualifier(from: &FromClause, qualifier: &str) -> bool {
    match from {
        FromClause::Table { .. } => false,
        FromClause::Join {
            left, right, on, ..
        } => {
            from_references_qualifier(left, qualifier)
                || from_references_qualifier(right, qualifier)
                || on
                    .as_ref()
                    .is_some_and(|expr| expr_references_qualifier(expr, qualifier))
        }
        FromClause::Values { rows, .. } => rows
            .iter()
            .flatten()
            .any(|expr| expr_references_qualifier(expr, qualifier)),
        FromClause::Function { args, .. } => args
            .iter()
            .any(|expr| expr_references_qualifier(expr, qualifier)),
        FromClause::FunctionGroup { functions, .. } => functions.iter().any(|function| {
            function
                .args
                .iter()
                .any(|expr| expr_references_qualifier(expr, qualifier))
        }),
        FromClause::Subquery { body, .. } => select_references_qualifier(body, qualifier),
    }
}

fn unique_rule_source_qualifier(statement: &Statement, preferred: &str) -> String {
    let mut names = BTreeSet::new();
    collect_statement_relation_names(statement, &mut names);
    let mut references = QualifiedReferenceCollector { names: &mut names };
    let _ = bind_statement(statement, &mut references);
    if !names.contains(&preferred.to_ascii_lowercase()) {
        return preferred.to_string();
    }
    for suffix in 1_u64.. {
        let candidate = format!("{preferred}_{suffix}");
        if !names.contains(&candidate.to_ascii_lowercase()) {
            return candidate;
        }
    }
    unreachable!("the finite SQL statement exhausted every rule source qualifier")
}

struct QualifiedReferenceCollector<'a> {
    names: &'a mut BTreeSet<String>,
}

impl VariableResolver for QualifiedReferenceCollector<'_> {
    fn resolve_name(&mut self, _name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        _column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        collect_name(qualifier, self.names);
        Ok(None)
    }

    fn resolve_param(&mut self, _index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }
}

fn collect_statement_relation_names(statement: &Statement, names: &mut BTreeSet<String>) {
    match statement {
        Statement::Select(select) => collect_select_relation_names(select, names),
        Statement::Insert(insert) => {
            collect_name(&insert.table, names);
            collect_name(&insert.target_qualifier, names);
            collect_name(&insert.returning_aliases.old, names);
            collect_name(&insert.returning_aliases.new, names);
            collect_cte_relation_names(&insert.with, names);
            if let Some(select) = &insert.select_source {
                collect_select_relation_names(select, names);
            }
        }
        Statement::Update(update) => {
            collect_name(&update.table, names);
            collect_name(&update.target_qualifier, names);
            collect_name(&update.returning_aliases.old, names);
            collect_name(&update.returning_aliases.new, names);
            collect_cte_relation_names(&update.with, names);
            if let Some(from) = &update.from {
                collect_from_relation_names(from, names);
            }
        }
        Statement::Delete(delete) => {
            collect_name(&delete.table, names);
            collect_name(&delete.target_qualifier, names);
            collect_name(&delete.returning_aliases.old, names);
            collect_name(&delete.returning_aliases.new, names);
            collect_cte_relation_names(&delete.with, names);
            if let Some(using) = &delete.using {
                collect_from_relation_names(using, names);
            }
        }
        _ => {}
    }
}

fn collect_cte_relation_names(ctes: &[uqa_sql::ast::CTE], names: &mut BTreeSet<String>) {
    for cte in ctes {
        collect_name(&cte.name, names);
        collect_select_relation_names(&cte.query, names);
    }
}

fn collect_select_relation_names(select: &SelectStmt, names: &mut BTreeSet<String>) {
    collect_cte_relation_names(&select.with, names);
    if let Some(from) = &select.from {
        collect_from_relation_names(from, names);
    }
    if let Some(set_op) = &select.set_op {
        if let Some(left) = &set_op.left {
            collect_select_relation_names(left, names);
        }
        collect_select_relation_names(&set_op.right, names);
    }
}

fn collect_from_relation_names(from: &FromClause, names: &mut BTreeSet<String>) {
    match from {
        FromClause::Table {
            name,
            qualifier,
            alias,
            ..
        } => {
            collect_name(name, names);
            collect_name(qualifier, names);
            if let Some(alias) = alias {
                collect_name(alias, names);
            }
        }
        FromClause::Join {
            left,
            right,
            alias,
            using,
            ..
        } => {
            collect_from_relation_names(left, names);
            collect_from_relation_names(right, names);
            if let Some(alias) = alias {
                collect_name(alias, names);
            }
            if let Some(alias) = using.as_ref().and_then(|using| using.alias.as_ref()) {
                collect_name(alias, names);
            }
        }
        FromClause::Values { alias, .. } => {
            if let Some(alias) = alias {
                collect_name(alias, names);
            }
        }
        FromClause::Function {
            name,
            output_name,
            alias,
            ..
        } => {
            collect_name(name, names);
            collect_name(output_name, names);
            if let Some(alias) = alias {
                collect_name(alias, names);
            }
        }
        FromClause::FunctionGroup {
            functions, alias, ..
        } => {
            for function in functions {
                collect_name(&function.name, names);
                collect_name(&function.output_name, names);
            }
            if let Some(alias) = alias {
                collect_name(alias, names);
            }
        }
        FromClause::Subquery { body, alias, .. } => {
            collect_select_relation_names(body, names);
            if let Some(alias) = alias {
                collect_name(alias, names);
            }
        }
    }
}

fn collect_name(name: &str, names: &mut BTreeSet<String>) {
    names.insert(name.to_ascii_lowercase());
}
