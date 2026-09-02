//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Trigger-function resolution and definition validation.

use uqa_core::Value;
use uqa_sql::ast::{
    ColumnDef, ColumnType, CreateRule, CreateTrigger, Expr, FromClause, FunctionReturns,
    OnConflictAction, RuleEvent, SelectStmt, Statement, TableHierarchy, TriggerEvent,
    TriggerTiming, TriggerTransitionRelation,
};
use uqa_sql::plpgsql::{bind_expr, ResolvedVariable, VariableResolver};
use uqa_sql::SQLError;

use super::{
    first_rule_row_reference_in_expr, first_rule_row_reference_in_select,
    rule_action_has_set_operation,
};
use crate::engine_capabilities::{RelationLookupMode, RelationResolution};
use crate::engine_user_functions::{
    canonical_routine_type_name, routine_signature_types, CompiledFunctionBody, SQLUserFunction,
};
use crate::{Arc, Engine, RelationIdentity, StoredViewKind};

impl Engine {
    pub(in crate::engine_events) fn event_relation_from_resolution(
        requested: &str,
        resolution: RelationResolution,
    ) -> Result<(RelationIdentity, &'static str), SQLError> {
        let (canonical, kind) = match resolution {
            RelationResolution::Found(canonical, kind)
                if matches!(kind, "table" | "view" | "materialized view") =>
            {
                (canonical, kind)
            }
            RelationResolution::Found(_, _) | RelationResolution::MissingRelation => {
                return Err(SQLError::UnknownTable(requested.to_string()));
            }
            RelationResolution::MissingSchema(schema) => {
                return Err(SQLError::Routine {
                    sqlstate: "3F000".into(),
                    message: format!("schema \"{schema}\" does not exist"),
                });
            }
        };
        let relation = RelationIdentity::from_legacy_name(&canonical).map_err(|error| {
            SQLError::Internal(format!(
                "decode resolved event relation `{canonical}`: {error}"
            ))
        })?;
        Ok((relation, kind))
    }

    pub(in crate::engine_events) fn resolve_event_relation_kind(
        &self,
        name: &str,
        lookup_mode: RelationLookupMode,
    ) -> Result<(RelationIdentity, &'static str), SQLError> {
        let resolution = match lookup_mode {
            RelationLookupMode::Dynamic => self.resolve_visible_relation_kind(name)?,
            RelationLookupMode::Bound => self.resolve_bound_relation_kind(name)?,
        };
        Self::event_relation_from_resolution(name, resolution)
    }
}

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

fn duplicate_rule_pseudo_relation(qualifier: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42712".into(),
        message: format!("table name \"{qualifier}\" specified more than once"),
    }
}

fn rule_pseudo_relation_name(name: &str) -> Option<String> {
    (name.eq_ignore_ascii_case("old") || name.eq_ignore_ascii_case("new"))
        .then(|| name.to_ascii_lowercase())
}

fn first_rule_pseudo_relation_in_from(from: &FromClause) -> Option<String> {
    match from {
        FromClause::Table {
            name,
            qualifier,
            alias,
            ..
        } => alias
            .as_deref()
            .and_then(rule_pseudo_relation_name)
            .or_else(|| rule_pseudo_relation_name(qualifier))
            .or_else(|| rule_pseudo_relation_name(name))
            .or_else(|| {
                name.rsplit_once('.')
                    .and_then(|(_, local)| rule_pseudo_relation_name(local.trim_matches('"')))
            }),
        FromClause::Join {
            left, right, alias, ..
        } => alias
            .as_deref()
            .and_then(rule_pseudo_relation_name)
            .or_else(|| first_rule_pseudo_relation_in_from(left))
            .or_else(|| first_rule_pseudo_relation_in_from(right)),
        FromClause::Values { alias, .. } | FromClause::Subquery { alias, .. } => {
            alias.as_deref().and_then(rule_pseudo_relation_name)
        }
        FromClause::Function {
            output_name, alias, ..
        } => rule_pseudo_relation_name(alias.as_deref().unwrap_or(output_name)),
        FromClause::FunctionGroup {
            functions, alias, ..
        } => alias
            .as_deref()
            .and_then(rule_pseudo_relation_name)
            .or_else(|| {
                functions
                    .iter()
                    .find_map(|function| rule_pseudo_relation_name(&function.output_name))
            }),
    }
}

fn validate_rule_action_select_namespace(select: &SelectStmt) -> Result<(), SQLError> {
    let duplicate = select
        .with
        .iter()
        .find_map(|cte| rule_pseudo_relation_name(&cte.name))
        .or_else(|| {
            select
                .from
                .as_ref()
                .and_then(first_rule_pseudo_relation_in_from)
        });
    if let Some(qualifier) = duplicate {
        return Err(duplicate_rule_pseudo_relation(&qualifier));
    }
    Ok(())
}

fn validate_rule_action_namespace(action: &Statement) -> Result<(), SQLError> {
    let (ctes, source) = match action {
        Statement::Select(select) => return validate_rule_action_select_namespace(select),
        Statement::Insert(insert) => (insert.with.as_slice(), insert.select_source.as_deref()),
        Statement::Update(update) => {
            if let Some(qualifier) = update
                .from
                .as_ref()
                .and_then(first_rule_pseudo_relation_in_from)
            {
                return Err(duplicate_rule_pseudo_relation(&qualifier));
            }
            (update.with.as_slice(), None)
        }
        Statement::Delete(delete) => {
            if let Some(qualifier) = delete
                .using
                .as_ref()
                .and_then(first_rule_pseudo_relation_in_from)
            {
                return Err(duplicate_rule_pseudo_relation(&qualifier));
            }
            (delete.with.as_slice(), None)
        }
        _ => return Ok(()),
    };
    if let Some(qualifier) = ctes
        .iter()
        .find_map(|cte| rule_pseudo_relation_name(&cte.name))
    {
        return Err(duplicate_rule_pseudo_relation(&qualifier));
    }
    if let Some(select) = source {
        validate_rule_action_select_namespace(select)?;
    }
    Ok(())
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
        | Expr::InternalColumn(_)
        | Expr::Param(_) => {}
    }
    Ok(())
}

fn validate_rule_action_reference_scopes(action: &Statement) -> Result<(), SQLError> {
    validate_rule_action_namespace(action)?;
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
                        .find_map(|(_, expr)| {
                            let mut shadowed = std::collections::BTreeSet::new();
                            shadowed.insert(insert.target_qualifier.to_ascii_lowercase());
                            first_rule_row_reference_in_expr(expr, &shadowed)
                        })
                        .or_else(|| {
                            r#where.as_ref().and_then(|expr| {
                                let mut shadowed = std::collections::BTreeSet::new();
                                shadowed.insert(insert.target_qualifier.to_ascii_lowercase());
                                first_rule_row_reference_in_expr(expr, &shadowed)
                            })
                        });
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

fn validate_trigger_transition_relation(
    definition: &CreateTrigger,
    hierarchy: &TableHierarchy,
    transition: &TriggerTransitionRelation,
) -> Result<(), SQLError> {
    if !transition.is_table {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "ROW variable naming in the REFERENCING clause is not supported".into(),
        });
    }
    if definition.row && !hierarchy.parents.is_empty() {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: if hierarchy.partition_bound.is_some() {
                "ROW triggers with transition tables are not supported on partitions".into()
            } else {
                "ROW triggers with transition tables are not supported on inheritance children"
                    .into()
            },
        });
    }
    if definition.timing != TriggerTiming::After {
        return Err(SQLError::Routine {
            sqlstate: "42P17".into(),
            message: "transition table name can only be specified for an AFTER trigger".into(),
        });
    }
    if definition.events.contains(&TriggerEvent::Truncate) {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "TRUNCATE triggers with transition tables are not supported".into(),
        });
    }
    let mutation_events = definition
        .events
        .iter()
        .filter(|event| {
            matches!(
                event,
                TriggerEvent::Insert | TriggerEvent::Update | TriggerEvent::Delete
            )
        })
        .count();
    if mutation_events != 1 {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "transition tables cannot be specified for triggers with more than one event"
                .into(),
        });
    }
    if !definition.update_columns.is_empty() {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "transition tables cannot be specified for triggers with column lists".into(),
        });
    }
    let valid_event = definition.events.iter().any(|event| {
        if transition.is_new {
            matches!(event, TriggerEvent::Insert | TriggerEvent::Update)
        } else {
            matches!(event, TriggerEvent::Delete | TriggerEvent::Update)
        }
    });
    if !valid_event {
        return Err(SQLError::Routine {
            sqlstate: "42P17".into(),
            message: format!(
                "{} TABLE can only be specified for {} trigger",
                if transition.is_new { "NEW" } else { "OLD" },
                if transition.is_new {
                    "an INSERT or UPDATE"
                } else {
                    "a DELETE or UPDATE"
                }
            ),
        });
    }
    Ok(())
}

mod rules;
mod triggers;
