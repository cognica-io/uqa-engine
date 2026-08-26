//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Trigger-function resolution and definition validation.

use uqa_core::Value;
use uqa_sql::ast::{
    ColumnType, CreateRule, CreateTrigger, Expr, FromClause, FunctionReturns, OnConflictAction,
    RuleEvent, SelectStmt, Statement, TriggerEvent, TriggerTiming,
};
use uqa_sql::plpgsql::{bind_expr, bind_select, ResolvedVariable, VariableResolver};
use uqa_sql::SQLError;

use crate::engine_user_functions::{
    canonical_routine_type_name, routine_signature_types, CompiledFunctionBody, SQLUserFunction,
};
use crate::{Arc, Engine, RelationIdentity, StoredViewKind};

struct TriggerConditionTypeResolver<'a> {
    columns: &'a [uqa_sql::ast::ColumnDef],
}

struct RuleRowTypeResolver<'a> {
    columns: &'a [(String, ColumnType)],
    event: RuleEvent,
}

struct RuleConditionNameResolver<'a> {
    columns: &'a [(String, ColumnType)],
    event: RuleEvent,
}

impl VariableResolver for RuleConditionNameResolver<'_> {
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

    fn rewrite_name(&mut self, name: &str) -> Result<Option<Expr>, SQLError> {
        if !self.columns.iter().any(|(column, _)| column == name) {
            return Err(SQLError::UnknownColumn(name.to_string()));
        }
        let qualifier = match self.event {
            RuleEvent::Insert => "new",
            RuleEvent::Delete => "old",
            RuleEvent::Update => return Err(SQLError::AmbiguousColumn(name.to_string())),
            RuleEvent::Select => return Ok(None),
        };
        Ok(Some(Expr::qualified_column(qualifier, name)))
    }
}

impl RuleRowTypeResolver<'_> {
    fn resolve_record_field(
        &self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        let is_old = qualifier.eq_ignore_ascii_case("old");
        let is_new = qualifier.eq_ignore_ascii_case("new");
        if !is_old && !is_new {
            return Ok(None);
        }
        if is_old && matches!(self.event, RuleEvent::Insert | RuleEvent::Select) {
            return Err(SQLError::Routine {
                sqlstate: "42P17".into(),
                message: format!(
                    "there is no OLD relation for {event} rule",
                    event = rule_event_name(self.event)
                ),
            });
        }
        if is_new && matches!(self.event, RuleEvent::Delete | RuleEvent::Select) {
            return Err(SQLError::Routine {
                sqlstate: "42P17".into(),
                message: format!(
                    "there is no NEW relation for {event} rule",
                    event = rule_event_name(self.event)
                ),
            });
        }
        let (_, ty) = self
            .columns
            .iter()
            .find(|(name, _)| name == column)
            .ok_or_else(|| SQLError::UnknownColumn(format!("{qualifier}.{column}")))?;
        Ok(Some(ResolvedVariable {
            value: Value::Null,
            declared_type: Some(ty.sql_name()),
        }))
    }
}

impl VariableResolver for RuleRowTypeResolver<'_> {
    fn resolve_name(&mut self, _name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        self.resolve_record_field(qualifier, column)
    }

    fn resolve_param(&mut self, _index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }
}

const fn rule_event_name(event: RuleEvent) -> &'static str {
    match event {
        RuleEvent::Select => "SELECT",
        RuleEvent::Insert => "INSERT",
        RuleEvent::Update => "UPDATE",
        RuleEvent::Delete => "DELETE",
    }
}

impl VariableResolver for TriggerConditionTypeResolver<'_> {
    fn resolve_name(&mut self, _name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        if !qualifier.eq_ignore_ascii_case("old") && !qualifier.eq_ignore_ascii_case("new") {
            return Ok(None);
        }
        Ok(self
            .columns
            .iter()
            .find(|definition| definition.name == column)
            .map(|definition| ResolvedVariable {
                value: Value::Null,
                declared_type: Some(definition.ty.sql_name()),
            }))
    }

    fn resolve_param(&mut self, _index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }
}

fn is_boolean_type(ty: &ColumnType) -> bool {
    match ty {
        ColumnType::Boolean => true,
        ColumnType::Domain { base, .. } => is_boolean_type(base),
        _ => false,
    }
}

fn rule_action_has_returning(action: &Statement) -> bool {
    match action {
        Statement::Insert(statement) => !statement.returning.is_empty(),
        Statement::Update(statement) => !statement.returning.is_empty(),
        Statement::Delete(statement) => !statement.returning.is_empty(),
        _ => false,
    }
}

fn rule_action_has_set_operation(action: &Statement) -> bool {
    match action {
        Statement::Select(select) => select.set_op.is_some(),
        Statement::Insert(insert) => insert
            .select_source
            .as_ref()
            .is_some_and(|select| select.set_op.is_some()),
        _ => false,
    }
}

fn same_rule_returning_type_with_different_modifier(
    actual: &ColumnType,
    expected: &ColumnType,
) -> bool {
    match (actual, expected) {
        (ColumnType::Varchar(_), ColumnType::Varchar(_))
        | (ColumnType::Character(_), ColumnType::Character(_))
        | (ColumnType::Numeric { .. }, ColumnType::Numeric { .. })
        | (ColumnType::Vector(_), ColumnType::Vector(_))
        | (ColumnType::Tensor(_), ColumnType::Tensor(_)) => true,
        (ColumnType::Array(actual), ColumnType::Array(expected)) => {
            same_rule_returning_type_with_different_modifier(actual, expected)
        }
        _ => false,
    }
}

fn validate_rule_returning_shape(
    schema: &uqa_execution::RowSchema,
    columns: &[(String, ColumnType)],
) -> Result<(), SQLError> {
    if schema.len() < columns.len() {
        return Err(SQLError::Routine {
            sqlstate: "42P17".into(),
            message: "RETURNING list has too few entries".into(),
        });
    }
    if schema.len() > columns.len() {
        return Err(SQLError::Routine {
            sqlstate: "42P17".into(),
            message: "RETURNING list has too many entries".into(),
        });
    }
    for (position, (column, expected)) in columns.iter().enumerate() {
        let Some(actual) = schema.column_type(position) else {
            // PostgreSQL resolves an unknown literal against the event row's
            // declared type when the rule target list is installed.
            continue;
        };
        if actual == expected {
            continue;
        }
        let difference = if same_rule_returning_type_with_different_modifier(actual, expected) {
            "size"
        } else {
            "type"
        };
        return Err(SQLError::Routine {
            sqlstate: "42P17".into(),
            message: format!(
                "RETURNING list's entry {} has different {difference} from column \"{column}\"\nDETAIL: RETURNING list entry has type {}, but column has type {}.",
                position + 1,
                actual.sql_name(),
                expected.sql_name()
            ),
        });
    }
    Ok(())
}

fn validate_trigger_condition_references(
    definition: &CreateTrigger,
    columns: &[uqa_sql::ast::ColumnDef],
    condition: &Expr,
) -> Result<(), SQLError> {
    if condition.any_node(&|node| {
        matches!(
            node,
            Expr::ScalarSubquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. }
        )
    }) {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "cannot use subquery in trigger WHEN condition".into(),
        });
    }
    if condition.any_node(&|node| matches!(node, Expr::Column(_))) {
        return Err(SQLError::Routine {
            sqlstate: "42P01".into(),
            message: "trigger WHEN condition must qualify row columns with OLD or NEW".into(),
        });
    }
    let references_old = condition.any_node(&|node| {
        matches!(node, Expr::QualifiedColumn { qualifier, .. } if qualifier.eq_ignore_ascii_case("old"))
    });
    let references_new = condition.any_node(&|node| {
        matches!(node, Expr::QualifiedColumn { qualifier, .. } if qualifier.eq_ignore_ascii_case("new"))
    });
    if !definition.row && (references_old || references_new) {
        return Err(SQLError::Routine {
            sqlstate: "42P01".into(),
            message: "statement trigger's WHEN condition cannot reference row values".into(),
        });
    }
    if references_old && definition.events.contains(&TriggerEvent::Insert) {
        return Err(SQLError::Routine {
            sqlstate: "42P17".into(),
            message: "INSERT trigger's WHEN condition cannot reference OLD values".into(),
        });
    }
    if references_new && definition.events.contains(&TriggerEvent::Delete) {
        return Err(SQLError::Routine {
            sqlstate: "42P17".into(),
            message: "DELETE trigger's WHEN condition cannot reference NEW values".into(),
        });
    }
    let invalid_qualified_reference = std::cell::RefCell::new(None);
    let _ = condition.any_node(&|node| {
        let Expr::QualifiedColumn { qualifier, column } = node else {
            return false;
        };
        if !qualifier.eq_ignore_ascii_case("old") && !qualifier.eq_ignore_ascii_case("new") {
            *invalid_qualified_reference.borrow_mut() = Some(format!("{qualifier}.{column}"));
            return true;
        }
        if !columns.iter().any(|definition| definition.name == *column) {
            *invalid_qualified_reference.borrow_mut() = Some(column.clone());
            return true;
        }
        false
    });
    if let Some(reference) = invalid_qualified_reference.into_inner() {
        return Err(SQLError::UnknownColumn(reference));
    }
    if definition.timing == TriggerTiming::Before && references_new {
        let generated = columns
            .iter()
            .filter(|column| column.generated.is_some())
            .map(|column| column.name.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        if condition.any_node(&|node| {
            matches!(
                node,
                Expr::QualifiedColumn { qualifier, column }
                    if qualifier.eq_ignore_ascii_case("new")
                        && generated.contains(column.as_str())
            )
        }) {
            return Err(SQLError::Routine {
                sqlstate: "42P17".into(),
                message: "BEFORE trigger's WHEN condition cannot reference NEW generated columns"
                    .into(),
            });
        }
    }
    Ok(())
}

fn first_invalid_rule_condition_qualifier(condition: &Expr) -> Option<String> {
    let invalid = std::cell::RefCell::new(None);
    let _ = condition.any_node(&|node| {
        let Expr::QualifiedColumn { qualifier, .. } = node else {
            return false;
        };
        if qualifier.eq_ignore_ascii_case("old") || qualifier.eq_ignore_ascii_case("new") {
            return false;
        }
        *invalid.borrow_mut() = Some(qualifier.clone());
        true
    });
    invalid.into_inner()
}

#[derive(Default)]
struct RuleRowReferenceDetector {
    qualifier: Option<String>,
}

impl VariableResolver for RuleRowReferenceDetector {
    fn resolve_name(&mut self, _name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        _column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        if self.qualifier.is_none()
            && (qualifier.eq_ignore_ascii_case("old") || qualifier.eq_ignore_ascii_case("new"))
        {
            self.qualifier = Some(qualifier.to_ascii_lowercase());
        }
        Ok(None)
    }

    fn resolve_param(&mut self, _index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }
}

fn first_rule_row_reference_in_expr(expr: &Expr) -> Option<String> {
    let mut detector = RuleRowReferenceDetector::default();
    let _ = bind_expr(expr, &mut detector);
    detector.qualifier
}

fn first_rule_row_reference_in_select(select: &SelectStmt) -> Option<String> {
    let mut detector = RuleRowReferenceDetector::default();
    let _ = bind_select(select, &mut detector);
    detector.qualifier
}

fn invalid_rule_cte_reference(qualifier: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "0A000".into(),
        message: format!(
            "cannot refer to {} within WITH query",
            qualifier.to_ascii_uppercase()
        ),
    }
}

fn invalid_rule_set_operation_reference() -> SQLError {
    SQLError::Routine {
        sqlstate: "42P10".into(),
        message:
            "UNION/INTERSECT/EXCEPT member statement cannot refer to other relations of same query level"
                .into(),
    }
}

fn invalid_rule_conflict_reference(qualifier: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42P01".into(),
        message: format!(
            "invalid reference to FROM-clause entry for table \"{qualifier}\"\nDETAIL: There is an entry for table \"{qualifier}\", but it cannot be referenced from this part of the query."
        ),
    }
}

fn validate_rule_ctes(ctes: &[uqa_sql::ast::CTE]) -> Result<(), SQLError> {
    for cte in ctes {
        if let Some(qualifier) = first_rule_row_reference_in_select(&cte.query) {
            return Err(invalid_rule_cte_reference(&qualifier));
        }
        validate_rule_select_scopes(&cte.query)?;
    }
    Ok(())
}

fn validate_rule_select_scopes(select: &SelectStmt) -> Result<(), SQLError> {
    validate_rule_ctes(&select.with)?;
    if let Some(set_op) = &select.set_op {
        let member_references_rule_row = set_op
            .left
            .as_deref()
            .and_then(first_rule_row_reference_in_select)
            .or_else(|| first_rule_row_reference_in_select(&set_op.right));
        if member_references_rule_row.is_some() {
            return Err(invalid_rule_set_operation_reference());
        }
        if let Some(left) = set_op.left.as_deref() {
            validate_rule_select_scopes(left)?;
        }
        validate_rule_select_scopes(&set_op.right)?;
        for order in &set_op.combined_order_by {
            validate_rule_expr_scopes(&order.expr)?;
        }
        if let Some(limit) = &set_op.combined_limit {
            validate_rule_expr_scopes(limit)?;
        }
        if let Some(offset) = &set_op.combined_offset {
            validate_rule_expr_scopes(offset)?;
        }
    }
    for projection in &select.projections {
        validate_rule_expr_scopes(&projection.expr)?;
    }
    for expr in select.values.iter().flatten() {
        validate_rule_expr_scopes(expr)?;
    }
    if let Some(from) = &select.from {
        validate_rule_from_scopes(from)?;
    }
    for expr in select
        .r#where
        .iter()
        .chain(select.group_by.iter())
        .chain(select.grouping_sets.iter().flatten())
        .chain(select.having.iter())
        .chain(select.order_by.iter().map(|order| &order.expr))
        .chain(select.limit.iter())
        .chain(select.offset.iter())
        .chain(select.distinct_on.iter())
    {
        validate_rule_expr_scopes(expr)?;
    }
    Ok(())
}

fn validate_rule_from_scopes(from: &FromClause) -> Result<(), SQLError> {
    match from {
        FromClause::Table { .. } => {}
        FromClause::Join {
            left, right, on, ..
        } => {
            validate_rule_from_scopes(left)?;
            validate_rule_from_scopes(right)?;
            if let Some(on) = on {
                validate_rule_expr_scopes(on)?;
            }
        }
        FromClause::Values { rows, .. } => {
            for expr in rows.iter().flatten() {
                validate_rule_expr_scopes(expr)?;
            }
        }
        FromClause::Function { args, .. } => {
            for expr in args {
                validate_rule_expr_scopes(expr)?;
            }
        }
        FromClause::FunctionGroup { functions, .. } => {
            for expr in functions.iter().flat_map(|function| &function.args) {
                validate_rule_expr_scopes(expr)?;
            }
        }
        FromClause::Subquery { body, .. } => validate_rule_select_scopes(body)?,
    }
    Ok(())
}

fn validate_rule_expr_scopes(expr: &Expr) -> Result<(), SQLError> {
    match expr {
        Expr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            for expr in args {
                validate_rule_expr_scopes(expr)?;
            }
            for order in order_by {
                validate_rule_expr_scopes(&order.expr)?;
            }
            if let Some(filter) = filter {
                validate_rule_expr_scopes(filter)?;
            }
        }
        Expr::Array(items) | Expr::Row(items) | Expr::And(items) | Expr::Or(items) => {
            for expr in items {
                validate_rule_expr_scopes(expr)?;
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            validate_rule_expr_scopes(lhs)?;
            validate_rule_expr_scopes(rhs)?;
        }
        Expr::UnaryMinus(expr)
        | Expr::Not(expr)
        | Expr::IsNull { expr, .. }
        | Expr::Cast { expr, .. } => validate_rule_expr_scopes(expr)?,
        Expr::Between { expr, low, high } => {
            validate_rule_expr_scopes(expr)?;
            validate_rule_expr_scopes(low)?;
            validate_rule_expr_scopes(high)?;
        }
        Expr::InList { expr, list, .. } => {
            validate_rule_expr_scopes(expr)?;
            for item in list {
                validate_rule_expr_scopes(item)?;
            }
        }
        Expr::WindowCall { args, spec, .. } => {
            for expr in args.iter().chain(spec.partition_by.iter()) {
                validate_rule_expr_scopes(expr)?;
            }
            for order in &spec.order_by {
                validate_rule_expr_scopes(&order.expr)?;
            }
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base {
                validate_rule_expr_scopes(base)?;
            }
            for (condition, result) in when {
                validate_rule_expr_scopes(condition)?;
                validate_rule_expr_scopes(result)?;
            }
            if let Some(else_branch) = else_branch {
                validate_rule_expr_scopes(else_branch)?;
            }
        }
        Expr::ScalarSubquery(body) | Expr::Exists { body, .. } => {
            validate_rule_select_scopes(body)?;
        }
        Expr::InSubquery { expr, body, .. } => {
            validate_rule_expr_scopes(expr)?;
            validate_rule_select_scopes(body)?;
        }
        Expr::Default
        | Expr::Literal(_)
        | Expr::Star
        | Expr::QualifiedStar(_)
        | Expr::Column(_)
        | Expr::QualifiedColumn { .. }
        | Expr::Param(_) => {}
    }
    Ok(())
}

fn validate_rule_action_reference_scopes(action: &Statement) -> Result<(), SQLError> {
    match action {
        Statement::Select(select) => validate_rule_select_scopes(select),
        Statement::Insert(insert) => {
            validate_rule_ctes(&insert.with)?;
            for expr in insert.rows.iter().flatten() {
                validate_rule_expr_scopes(expr)?;
            }
            if let Some(select) = &insert.select_source {
                validate_rule_select_scopes(select)?;
            }
            if let Some(conflict) = &insert.on_conflict {
                if let OnConflictAction::Update {
                    assignments,
                    r#where,
                } = &conflict.action
                {
                    let reference = assignments
                        .iter()
                        .find_map(|(_, expr)| first_rule_row_reference_in_expr(expr))
                        .or_else(|| r#where.as_ref().and_then(first_rule_row_reference_in_expr));
                    if let Some(qualifier) = reference {
                        return Err(invalid_rule_conflict_reference(&qualifier));
                    }
                    for (_, expr) in assignments {
                        validate_rule_expr_scopes(expr)?;
                    }
                    if let Some(r#where) = r#where {
                        validate_rule_expr_scopes(r#where)?;
                    }
                }
            }
            for projection in &insert.returning {
                validate_rule_expr_scopes(&projection.expr)?;
            }
            Ok(())
        }
        Statement::Update(update) => {
            validate_rule_ctes(&update.with)?;
            if let Some(from) = &update.from {
                validate_rule_from_scopes(from)?;
            }
            for expr in update
                .assignments
                .iter()
                .map(|(_, expr)| expr)
                .chain(update.r#where.iter())
                .chain(update.returning.iter().map(|projection| &projection.expr))
            {
                validate_rule_expr_scopes(expr)?;
            }
            Ok(())
        }
        Statement::Delete(delete) => {
            validate_rule_ctes(&delete.with)?;
            if let Some(using) = &delete.using {
                validate_rule_from_scopes(using)?;
            }
            for expr in delete
                .r#where
                .iter()
                .chain(delete.returning.iter().map(|projection| &projection.expr))
            {
                validate_rule_expr_scopes(expr)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

impl Engine {
    pub(crate) fn resolve_rule_relation(&self, name: &str) -> Result<RelationIdentity, SQLError> {
        if let Ok(relation) = RelationIdentity::from_legacy_name(name) {
            if self.durable.views.read().contains_key(&relation) {
                return Ok(relation);
            }
        }
        let table = self.try_resolve_table_name(name).map_err(|error| {
            SQLError::Internal(format!("resolve rule relation `{name}`: {error}"))
        })?;
        if let Some(table) = table {
            return RelationIdentity::from_legacy_name(&table).map_err(|error| {
                SQLError::Internal(format!("decode rule relation `{table}`: {error}"))
            });
        }
        let canonical = self
            .try_resolve_view_name(name)
            .map_err(|error| {
                SQLError::Internal(format!("resolve rule relation `{name}`: {error}"))
            })?
            .ok_or_else(|| SQLError::UnknownTable(name.to_string()))?;
        RelationIdentity::from_legacy_name(&canonical).map_err(|error| {
            SQLError::Internal(format!("decode rule relation `{canonical}`: {error}"))
        })
    }

    pub(crate) fn rule_relation_columns(
        &self,
        name: &str,
    ) -> Result<Vec<(String, ColumnType)>, SQLError> {
        if let Some(columns) = self
            .try_describe_table_row_type(name)
            .map_err(|error| SQLError::Internal(format!("read rule columns: {error}")))?
        {
            return Ok(columns
                .into_iter()
                .map(|column| (column.name, column.ty))
                .collect());
        }
        let relation = RelationIdentity::from_legacy_name(name).map_err(|error| {
            SQLError::Internal(format!("decode rule relation `{name}`: {error}"))
        })?;
        let view = self
            .durable
            .views
            .read()
            .get(&relation)
            .cloned()
            .ok_or_else(|| SQLError::UnknownTable(name.to_string()))?;
        let schema = self.stored_view_schema(&view)?;
        Ok(schema
            .columns()
            .iter()
            .enumerate()
            .map(|(index, name)| {
                (
                    schema.public_name(index).unwrap_or(name).to_string(),
                    schema
                        .column_type(index)
                        .cloned()
                        .unwrap_or(ColumnType::Text),
                )
            })
            .collect())
    }

    fn validate_select_rule_contract(
        definition: &CreateRule,
        is_view: bool,
    ) -> Result<(), SQLError> {
        if definition.event != RuleEvent::Select && definition.name == "_RETURN" {
            let relation =
                RelationIdentity::from_legacy_name(&definition.table).map_err(|error| {
                    SQLError::Internal(format!(
                        "decode rule relation `{}`: {error}",
                        definition.table
                    ))
                })?;
            return Err(SQLError::Routine {
                sqlstate: "42P17".into(),
                message: format!(
                    "non-view rule for \"{}\" must not be named \"_RETURN\"",
                    relation.name
                ),
            });
        }
        if definition.event == RuleEvent::Select {
            if !is_view {
                return Err(SQLError::Routine {
                    sqlstate: "42809".into(),
                    message: format!(
                        "relation \"{}\" cannot have ON SELECT rules",
                        definition.table
                    ),
                });
            }
            if definition.name != "_RETURN"
                || !definition.instead
                || definition.condition.is_some()
                || !matches!(definition.actions.as_slice(), [Statement::Select(_)])
            {
                return Err(SQLError::Routine {
                    sqlstate: "42P17".into(),
                    message: "view rule must be named \"_RETURN\", unconditional, INSTEAD, and have one SELECT action".into(),
                });
            }
        }
        Ok(())
    }

    fn validate_rule_condition(
        &self,
        condition: &mut Expr,
        columns: &[(String, ColumnType)],
        event: RuleEvent,
    ) -> Result<(), SQLError> {
        if condition.any_node(&|node| {
            matches!(
                node,
                Expr::ScalarSubquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. }
            )
        }) {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "cannot use subquery in rule WHERE condition".into(),
            });
        }
        if condition.contains_aggregate()
            || condition.contains_window()
            || condition.any_node(&|node| {
                matches!(node, Expr::Func { name, .. } if self.has_registered_aggregate_function(name))
            })
        {
            return Err(SQLError::Routine {
                sqlstate: "42803".into(),
                message: "aggregate and window functions are not allowed in rule WHERE conditions"
                    .into(),
            });
        }
        if condition.any_node(&|node| matches!(node, Expr::Column(_))) {
            *condition = bind_expr(condition, &mut RuleConditionNameResolver { columns, event })?;
        }
        if let Some(reference) = first_invalid_rule_condition_qualifier(condition) {
            return Err(SQLError::Routine {
                sqlstate: "42P10".into(),
                message: format!("rule WHERE condition cannot refer to relation \"{reference}\""),
            });
        }
        let bound = bind_expr(condition, &mut RuleRowTypeResolver { columns, event })?;
        let lowered = uqa_planner::ExpressionPlan::lower(bound);
        match uqa_execution::common_context_expression_type(
            &lowered.scalar,
            &uqa_execution::RowSchema::default(),
            &[],
            Some(self),
        )? {
            Some(ty) if !is_boolean_type(&ty) => {
                return Err(SQLError::TypeMismatch(format!(
                    "argument of WHERE must be type boolean, not type {}",
                    ty.sql_name()
                )))
            }
            None => {
                if let Expr::Literal(value @ (Value::Str(_) | Value::FixedChar(_))) = condition {
                    *value = uqa_sql::expr::cast_value(value, "boolean")?;
                } else {
                    *condition = Expr::Cast {
                        expr: Box::new(condition.clone()),
                        ty: "boolean".into(),
                    };
                }
            }
            Some(_) => {}
        }
        Ok(())
    }

    fn canonicalize_rule_action_target(&self, action: &mut Statement) -> Result<(), SQLError> {
        let target = match action {
            Statement::Insert(statement) => &mut statement.table,
            Statement::Update(statement) => &mut statement.table,
            Statement::Delete(statement) => &mut statement.table,
            _ => return Ok(()),
        };
        *target = self.resolve_rule_relation(target)?.qualified_name();
        Ok(())
    }

    fn rule_action_target_columns(
        &self,
        action: &Statement,
    ) -> Result<std::collections::BTreeSet<String>, SQLError> {
        let table = match action {
            Statement::Insert(statement) => &statement.table,
            Statement::Update(statement) => &statement.table,
            Statement::Delete(statement) => &statement.table,
            _ => return Ok(std::collections::BTreeSet::new()),
        };
        Ok(self
            .rule_relation_columns(table)?
            .into_iter()
            .map(|(column, _)| column)
            .collect())
    }

    pub(super) fn validate_rule_definition(
        &self,
        definition: &mut CreateRule,
    ) -> Result<RelationIdentity, SQLError> {
        let relation = self.resolve_rule_relation(&definition.table)?;
        definition.table = relation.qualified_name();
        let stored_view_kind = self
            .durable
            .views
            .read()
            .get(&relation)
            .map(|view| view.kind);
        if stored_view_kind == Some(StoredViewKind::Materialized) {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "rules on materialized views are not supported".into(),
            });
        }
        let is_view = stored_view_kind == Some(StoredViewKind::View);
        Self::validate_select_rule_contract(definition, is_view)?;
        let columns = self.rule_relation_columns(&definition.table)?;
        if let Some(condition) = definition.condition.as_mut() {
            self.validate_rule_condition(condition, &columns, definition.event)?;
        }
        if definition.condition.is_some()
            && definition.actions.iter().any(rule_action_has_set_operation)
        {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "conditional UNION/INTERSECT/EXCEPT statements are not implemented".into(),
            });
        }
        let returning_actions = definition
            .actions
            .iter()
            .filter(|action| rule_action_has_returning(action))
            .count();
        if returning_actions > 1 {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "cannot have multiple RETURNING lists in a rule".into(),
            });
        }
        if returning_actions != 0 && definition.condition.is_some() {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "RETURNING lists are not supported in conditional rules".into(),
            });
        }
        if returning_actions != 0 && !definition.instead {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "RETURNING lists are not supported in non-INSTEAD rules".into(),
            });
        }
        for action in &mut definition.actions {
            self.canonicalize_rule_action_target(action)?;
            validate_rule_action_reference_scopes(action)?;
            let action_columns = self.rule_action_target_columns(action)?;
            let bound = crate::engine_events::bind_rule_action(
                action,
                &action_columns,
                &mut RuleRowTypeResolver {
                    columns: &columns,
                    event: definition.event,
                },
            )?;
            if let Some(schema) = crate::sql::analyze_rule_action_returning_schema(self, bound)? {
                validate_rule_returning_shape(&schema, &columns)?;
            }
        }
        Ok(relation)
    }

    pub(super) fn resolve_trigger_table(&self, name: &str) -> Result<RelationIdentity, SQLError> {
        let canonical = self
            .try_resolve_table_name(name)
            .map_err(|error| {
                SQLError::Internal(format!("resolve trigger relation `{name}`: {error}"))
            })?
            .ok_or_else(|| SQLError::UnknownTable(name.to_string()))?;
        RelationIdentity::from_legacy_name(&canonical).map_err(|error| {
            SQLError::Internal(format!("decode trigger relation `{canonical}`: {error}"))
        })
    }

    pub(crate) fn resolve_trigger_function(
        &self,
        name: &str,
    ) -> Result<Arc<SQLUserFunction>, SQLError> {
        let candidates = self
            .lookup_sql_functions(name)
            .unwrap_or_default()
            .into_iter()
            .filter(|function| {
                !function.def.is_procedure && routine_signature_types(&function.def).is_empty()
            })
            .collect::<Vec<_>>();
        let function = match candidates.as_slice() {
            [function] => function.clone(),
            [] => {
                return Err(SQLError::Routine {
                    sqlstate: "42883".into(),
                    message: format!("function {name}() does not exist"),
                })
            }
            _ => {
                return Err(SQLError::Routine {
                    sqlstate: "42725".into(),
                    message: format!("function name \"{name}\" is not unique"),
                })
            }
        };
        let returns_trigger = matches!(
            &function.def.returns,
            FunctionReturns::Scalar { type_name }
                if canonical_routine_type_name(type_name) == "trigger"
        );
        if !returns_trigger {
            return Err(SQLError::Routine {
                sqlstate: "42P17".into(),
                message: format!("function {} must return type trigger", function.def.name),
            });
        }
        if !matches!(function.compiled, CompiledFunctionBody::PLpgSQL(_)) {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "only LANGUAGE plpgsql trigger functions are executable".into(),
            });
        }
        Ok(function)
    }

    pub(super) fn validate_trigger_definition(
        &self,
        definition: &mut CreateTrigger,
    ) -> Result<RelationIdentity, SQLError> {
        let relation = self.resolve_trigger_table(&definition.table)?;
        definition.table = relation.qualified_name();
        definition.function.clone_from(
            &self
                .resolve_trigger_function(&definition.function)?
                .def
                .name,
        );
        let columns = self
            .try_describe_table(&definition.table)
            .map_err(|error| SQLError::Internal(format!("read trigger columns: {error}")))?
            .ok_or_else(|| SQLError::UnknownTable(definition.table.clone()))?;
        if definition.events.contains(&TriggerEvent::Truncate) && definition.row {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "TRUNCATE FOR EACH ROW triggers are not supported".into(),
            });
        }
        if !definition.update_columns.is_empty()
            && !definition.events.contains(&TriggerEvent::Update)
        {
            return Err(SQLError::Routine {
                sqlstate: "42601".into(),
                message: "UPDATE OF columns may only be specified for an UPDATE trigger".into(),
            });
        }
        let mut seen_update_columns = std::collections::BTreeSet::new();
        for column in &definition.update_columns {
            if !columns.iter().any(|definition| definition.name == *column) {
                return Err(SQLError::UnknownColumn(format!(
                    "{}.{column}",
                    definition.table
                )));
            }
            if !seen_update_columns.insert(column) {
                return Err(SQLError::Routine {
                    sqlstate: "42701".into(),
                    message: format!("column \"{column}\" specified more than once"),
                });
            }
        }
        if let Some(mut condition) = definition.when.take() {
            self.validate_trigger_condition(definition, &columns, &mut condition)?;
            definition.when = Some(condition);
        }
        Ok(relation)
    }

    fn validate_trigger_condition(
        &self,
        definition: &CreateTrigger,
        columns: &[uqa_sql::ast::ColumnDef],
        condition: &mut Expr,
    ) -> Result<(), SQLError> {
        validate_trigger_condition_references(definition, columns, condition)?;
        let bound = bind_expr(condition, &mut TriggerConditionTypeResolver { columns })?;
        let lowered = uqa_planner::ExpressionPlan::lower(bound);
        match uqa_execution::common_context_expression_type(
            &lowered.scalar,
            &uqa_execution::RowSchema::default(),
            &[],
            Some(self),
        )? {
            Some(ty) if !is_boolean_type(&ty) => {
                return Err(SQLError::TypeMismatch(format!(
                    "argument of WHEN must be type boolean, not type {}",
                    ty.sql_name()
                )))
            }
            None => {
                if let Expr::Literal(value @ (Value::Str(_) | Value::FixedChar(_))) = condition {
                    *value = uqa_sql::expr::cast_value(value, "boolean")?;
                } else {
                    *condition = Expr::Cast {
                        expr: Box::new(condition.clone()),
                        ty: "boolean".into(),
                    };
                }
            }
            Some(_) => {}
        }
        Ok(())
    }
}
