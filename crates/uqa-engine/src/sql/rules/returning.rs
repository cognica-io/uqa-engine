//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rewrite-rule RETURNING validation, provider capture, and outer projection.

use std::collections::BTreeSet;

use uqa_core::Value;
use uqa_execution::{OwnedPhysicalRow, ScalarExpr};
use uqa_planner::{ProjectionPlan, QueryPlan, RelationalPlan, SourcePlan};
use uqa_sql::ast::{Expr, Projection, ReturningAliases, RuleEvent, Statement};
use uqa_sql::plpgsql::{ResolvedVariable, VariableResolver};
use uqa_sql::{SQLError, SQLResult};

use crate::sql::dml::{
    build_returning_value_row, dml_returning_result, DmlReturningShape, ReturningValueProjectionRow,
};
use crate::{Engine, RelationIdentity};

use super::RuleRowImage;

#[derive(Clone, Copy, Default)]
pub(in crate::sql) struct RuleReturningRequest {
    capture: bool,
    images: RuleReturningImages,
}

#[derive(Clone, Copy, Default)]
struct RuleReturningImages(u8);

impl RuleReturningImages {
    const CURRENT: Self = Self(1 << 0);
    const OLD: Self = Self(1 << 1);
    const NEW: Self = Self(1 << 2);

    fn insert(&mut self, image: Self) {
        self.0 |= image.0;
    }

    const fn contains(self, image: Self) -> bool {
        self.0 & image.0 != 0
    }
}

impl RuleReturningRequest {
    pub(in crate::sql) fn from_plan(
        returning: &[ProjectionPlan],
        aliases: &ReturningAliases,
        subqueries: &[QueryPlan],
    ) -> Self {
        if returning.is_empty() {
            return Self::default();
        }
        let mut request = Self {
            capture: true,
            ..Self::default()
        };
        let shadowed = BTreeSet::new();
        for projection in returning {
            let ids = request.inspect_expression(&projection.expr, aliases, &shadowed);
            for id in ids {
                if let Some(query) = subqueries.get(id) {
                    request.inspect_query(query, aliases, &shadowed);
                }
            }
        }
        request
    }

    fn inspect_expression(
        &mut self,
        expression: &ScalarExpr,
        aliases: &ReturningAliases,
        shadowed: &BTreeSet<String>,
    ) -> Vec<usize> {
        let mut expression = expression.clone();
        let mut subqueries = Vec::new();
        uqa_planner::rewrite_scalar_expression(&mut expression, &mut |node| match node {
            ScalarExpr::Star | ScalarExpr::Column(_) | ScalarExpr::Position(_) => {
                self.images.insert(RuleReturningImages::CURRENT);
            }
            ScalarExpr::QualifiedStar(qualifier)
            | ScalarExpr::QualifiedColumn { qualifier, .. }
                if !shadowed.contains(&qualifier.to_ascii_lowercase()) =>
            {
                if qualifier.eq_ignore_ascii_case(&aliases.old) {
                    self.images.insert(RuleReturningImages::OLD);
                } else if qualifier.eq_ignore_ascii_case(&aliases.new) {
                    self.images.insert(RuleReturningImages::NEW);
                } else {
                    self.images.insert(RuleReturningImages::CURRENT);
                }
            }
            ScalarExpr::ScalarSubquery(id)
            | ScalarExpr::Exists { subquery: id, .. }
            | ScalarExpr::InSubquery { subquery: id, .. } => subqueries.push(*id),
            _ => {}
        });
        subqueries
    }

    fn inspect_query(
        &mut self,
        query: &QueryPlan,
        aliases: &ReturningAliases,
        inherited: &BTreeSet<String>,
    ) {
        for cte in &query.ctes {
            self.inspect_query(&cte.query, aliases, inherited);
        }
        match &query.root {
            RelationalPlan::QueryBlock(block) => {
                let mut scope = inherited.clone();
                if let Some(source) = &block.from {
                    collect_source_qualifiers(source, &mut scope);
                    self.inspect_source(source, aliases, inherited);
                }
                for expression in block
                    .projections
                    .iter()
                    .map(|projection| &projection.expr)
                    .chain(block.r#where.iter())
                    .chain(block.group_by.iter())
                    .chain(block.grouping_sets.iter().flatten())
                    .chain(block.having.iter())
                    .chain(block.order_by.iter().map(|order| &order.expr))
                    .chain(block.limit.iter())
                    .chain(block.offset.iter())
                    .chain(block.distinct_on.iter())
                {
                    let _ = self.inspect_expression(expression, aliases, &scope);
                }
                for subquery in &block.subqueries {
                    self.inspect_query(subquery, aliases, &scope);
                }
            }
            RelationalPlan::SetOp {
                left,
                right,
                order_by,
                limit,
                offset,
                subqueries,
                ..
            } => {
                self.inspect_query(left, aliases, inherited);
                self.inspect_query(right, aliases, inherited);
                for expression in order_by
                    .iter()
                    .map(|order| &order.expr)
                    .chain(limit.iter().map(Box::as_ref))
                    .chain(offset.iter().map(Box::as_ref))
                {
                    let _ = self.inspect_expression(expression, aliases, inherited);
                }
                for subquery in subqueries {
                    self.inspect_query(subquery, aliases, inherited);
                }
            }
            RelationalPlan::Values { rows, subqueries } => {
                for expression in rows.iter().flatten() {
                    let _ = self.inspect_expression(expression, aliases, inherited);
                }
                for subquery in subqueries {
                    self.inspect_query(subquery, aliases, inherited);
                }
            }
        }
    }

    fn inspect_source(
        &mut self,
        source: &SourcePlan,
        aliases: &ReturningAliases,
        inherited: &BTreeSet<String>,
    ) {
        match source {
            SourcePlan::Table { .. } => {}
            SourcePlan::Join {
                left,
                right,
                on,
                lateral,
                ..
            } => {
                self.inspect_source(left, aliases, inherited);
                let mut right_scope = inherited.clone();
                if *lateral {
                    collect_source_qualifiers(left, &mut right_scope);
                }
                self.inspect_source(right, aliases, &right_scope);
                if let Some(on) = on {
                    let mut scope = inherited.clone();
                    collect_source_qualifiers(left, &mut scope);
                    collect_source_qualifiers(right, &mut scope);
                    let _ = self.inspect_expression(on, aliases, &scope);
                }
            }
            SourcePlan::Values { rows, .. } => {
                for expression in rows.iter().flatten() {
                    let _ = self.inspect_expression(expression, aliases, inherited);
                }
            }
            SourcePlan::Function { args, .. } => {
                for expression in args {
                    let _ = self.inspect_expression(expression, aliases, inherited);
                }
            }
            SourcePlan::FunctionGroup { functions, .. } => {
                for expression in functions.iter().flat_map(|function| &function.args) {
                    let _ = self.inspect_expression(expression, aliases, inherited);
                }
            }
            SourcePlan::Subquery { body, .. } => self.inspect_query(body, aliases, inherited),
        }
    }

    pub(super) const fn captures(self) -> bool {
        self.capture
    }
}

fn collect_source_qualifiers(source: &SourcePlan, output: &mut BTreeSet<String>) {
    match source {
        SourcePlan::Join {
            left, right, alias, ..
        } => {
            if let Some(alias) = alias {
                output.insert(alias.to_ascii_lowercase());
            } else {
                collect_source_qualifiers(left, output);
                collect_source_qualifiers(right, output);
            }
        }
        _ => {
            if let Some(qualifier) = source.visible_qualifier() {
                output.insert(qualifier.to_ascii_lowercase());
            }
        }
    }
}

pub(in crate::sql) struct RuleReturningResult {
    current: Vec<Vec<Value>>,
    old: Vec<Vec<Value>>,
    new: Vec<Vec<Value>>,
    contexts: Vec<Option<OwnedPhysicalRow>>,
    affected_rows: u64,
}

impl RuleReturningResult {
    pub(super) fn empty() -> Self {
        Self {
            current: Vec::new(),
            old: Vec::new(),
            new: Vec::new(),
            contexts: Vec::new(),
            affected_rows: 0,
        }
    }

    pub(in crate::sql) fn project(
        self,
        engine: &Engine,
        shape: DmlReturningShape<'_>,
    ) -> Result<SQLResult, SQLError> {
        if self.current.len() != self.old.len()
            || self.current.len() != self.new.len()
            || self.current.len() != self.contexts.len()
        {
            return Err(SQLError::Internal(
                "rewrite-rule RETURNING images and source contexts have different cardinalities"
                    .into(),
            ));
        }
        let mut rows = Vec::with_capacity(self.current.len());
        for index in 0..self.current.len() {
            rows.push(build_returning_value_row(
                engine,
                ReturningValueProjectionRow {
                    table: shape.table,
                    target_qualifier: shape.target_qualifier,
                    current: &self.current[index],
                    old: Some(&self.old[index]),
                    new: Some(&self.new[index]),
                    aliases: shape.aliases,
                    context: self.contexts[index].as_ref(),
                },
                shape.returning,
                shape.params,
                shape.ctes,
            )?);
        }
        dml_returning_result(engine, shape, rows, self.affected_rows)
    }
}

pub(in crate::sql) fn validate_rule_returning_contract(
    engine: &Engine,
    table: &str,
    event: RuleEvent,
    requested: bool,
) -> Result<(), SQLError> {
    if !requested {
        return Ok(());
    }
    let table = engine.resolve_rule_relation(table)?.qualified_name();
    let rules = engine.rules_for(&table, event)?;
    if rules.is_empty() || !rules.iter().any(|rule| rule.definition.instead) {
        return Ok(());
    }
    let providers = rules
        .iter()
        .flat_map(|rule| &rule.definition.actions)
        .filter(|action| statement_has_returning(action))
        .count();
    if providers > 1 {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "cannot have RETURNING lists in multiple rules".into(),
        });
    }
    if providers == 1 {
        return Ok(());
    }
    let relation = RelationIdentity::from_legacy_name(&table)
        .map_err(|error| SQLError::Internal(format!("decode rule relation `{table}`: {error}")))?;
    let event = rule_event_name(event);
    Err(SQLError::Routine {
        sqlstate: "0A000".into(),
        message: format!(
            "cannot perform {event} RETURNING on relation \"{}\"\nHINT: You need an unconditional ON {event} DO INSTEAD rule with a RETURNING clause.",
            relation.name
        ),
    })
}

const fn rule_event_name(event: RuleEvent) -> &'static str {
    match event {
        RuleEvent::Select => "SELECT",
        RuleEvent::Insert => "INSERT",
        RuleEvent::Update => "UPDATE",
        RuleEvent::Delete => "DELETE",
    }
}

pub(super) fn statement_has_returning(statement: &Statement) -> bool {
    match statement {
        Statement::Insert(statement) => !statement.returning.is_empty(),
        Statement::Update(statement) => !statement.returning.is_empty(),
        Statement::Delete(statement) => !statement.returning.is_empty(),
        _ => false,
    }
}

pub(super) fn clear_statement_returning(statement: &mut Statement) {
    match statement {
        Statement::Insert(statement) => statement.returning.clear(),
        Statement::Update(statement) => statement.returning.clear(),
        Statement::Delete(statement) => statement.returning.clear(),
        _ => {}
    }
}

pub(super) fn rule_returning_columns(
    engine: &Engine,
    table: &str,
) -> Result<Vec<uqa_sql::ast::ColumnDef>, SQLError> {
    let columns = engine
        .try_describe_table_row_type(table)
        .map_err(|error| SQLError::Internal(format!("read rule RETURNING row type: {error}")))?;
    if let Some(columns) = columns {
        return Ok(columns);
    }
    Ok(engine
        .rule_relation_columns(table)?
        .into_iter()
        .map(|(name, ty)| uqa_sql::ast::ColumnDef {
            name,
            ty,
            object_id: None,
            missing_value: None,
            primary_key: false,
            not_null: false,
            not_null_explicit: false,
            not_null_name: None,
            not_null_validated: true,
            not_null_no_inherit: false,
            auto_increment: None,
            unique: false,
            default: None,
            generated: None,
            check: None,
            check_name: None,
            check_enforced: true,
            check_validated: true,
            check_no_inherit: false,
            references: None,
        })
        .collect())
}

pub(super) fn capture_rule_returning_result(
    result: SQLResult,
    columns: &[uqa_sql::ast::ColumnDef],
    source_rows: Option<&[RuleRowImage]>,
) -> Result<RuleReturningResult, SQLError> {
    let width = columns.len();
    let image_width = width.saturating_mul(3);
    let expected_width = image_width + usize::from(source_rows.is_some());
    if result.columns.len() != expected_width {
        return Err(SQLError::Internal(format!(
            "rewrite-rule RETURNING provider produced {} columns, expected {}",
            result.columns.len(),
            expected_width
        )));
    }
    let extract = |row: usize, offset: usize| {
        columns
            .iter()
            .enumerate()
            .map(|(position, column)| {
                let value = result
                    .value_at(row, offset + position)
                    .cloned()
                    .ok_or_else(|| {
                        SQLError::Internal(
                            "rewrite-rule RETURNING provider lost a positional value".into(),
                        )
                    })?;
                crate::sql::convert_value_to_column_type(value, &column.ty)
            })
            .collect::<Result<Vec<_>, SQLError>>()
    };
    let mut current = Vec::with_capacity(result.rows.len());
    let mut old = Vec::with_capacity(result.rows.len());
    let mut new = Vec::with_capacity(result.rows.len());
    let mut contexts = Vec::with_capacity(result.rows.len());
    for row in 0..result.rows.len() {
        current.push(extract(row, 0)?);
        old.push(extract(row, width)?);
        new.push(extract(row, width * 2)?);
        let context = source_rows
            .map(|source_rows| {
                let source_index = match result.value_at(row, image_width) {
                    Some(Value::Int(index)) if *index >= 0 => usize::try_from(*index).map_err(|_| {
                        SQLError::Internal(
                            "rewrite-rule RETURNING source index exceeds usize".into(),
                        )
                    })?,
                    Some(value) => {
                        return Err(SQLError::Internal(format!(
                            "rewrite-rule RETURNING source index is not a non-negative integer: {value:?}"
                        )))
                    }
                    None => {
                        return Err(SQLError::Internal(
                            "rewrite-rule RETURNING provider lost its source index".into(),
                        ))
                    }
                };
                source_rows
                    .get(source_index)
                    .map(|source| source.context.clone())
                    .ok_or_else(|| {
                        SQLError::Internal(
                            "rewrite-rule RETURNING source index is outside the event row set"
                                .into(),
                        )
                    })
            })
            .transpose()?
            .flatten();
        contexts.push(context);
    }
    Ok(RuleReturningResult {
        current,
        old,
        new,
        contexts,
        affected_rows: result.affected_rows,
    })
}

pub(super) fn augment_rule_returning_action(
    statement: &mut Statement,
    source_index: Option<Expr>,
    event_width: usize,
    request: RuleReturningRequest,
    target_columns: &BTreeSet<String>,
) -> Result<(), SQLError> {
    let (target_qualifier, aliases, returning) = match statement {
        Statement::Insert(action) => (
            action.target_qualifier.clone(),
            action.returning_aliases.clone(),
            action.returning.clone(),
        ),
        Statement::Update(action) => (
            action.target_qualifier.clone(),
            action.returning_aliases.clone(),
            action.returning.clone(),
        ),
        Statement::Delete(action) => (
            action.target_qualifier.clone(),
            action.returning_aliases.clone(),
            action.returning.clone(),
        ),
        _ => return Ok(()),
    };
    if returning.is_empty() {
        return Ok(());
    }
    let mut target_columns = target_columns.clone();
    target_columns.insert(crate::sql::DOC_ID_COLUMN.into());
    let provider_event = match statement {
        Statement::Insert(_) => RuleEvent::Insert,
        Statement::Update(_) => RuleEvent::Update,
        Statement::Delete(_) => RuleEvent::Delete,
        _ => unreachable!("validated rule provider changed statement kind"),
    };
    let current = if request.images.contains(RuleReturningImages::CURRENT) {
        returning.clone()
    } else {
        null_rule_returning_image(event_width)
    };
    let old = if request.images.contains(RuleReturningImages::OLD)
        && provider_event != RuleEvent::Insert
    {
        rewrite_rule_returning_image(
            &returning,
            &target_qualifier,
            &target_columns,
            &aliases.old,
            &aliases.new,
            &aliases.old,
        )?
    } else {
        null_rule_returning_image(event_width)
    };
    let new = if request.images.contains(RuleReturningImages::NEW)
        && provider_event != RuleEvent::Delete
    {
        rewrite_rule_returning_image(
            &returning,
            &target_qualifier,
            &target_columns,
            &aliases.old,
            &aliases.new,
            &aliases.new,
        )?
    } else {
        null_rule_returning_image(event_width)
    };
    let output = match statement {
        Statement::Insert(action) => &mut action.returning,
        Statement::Update(action) => &mut action.returning,
        Statement::Delete(action) => &mut action.returning,
        _ => unreachable!("validated rule provider changed statement kind"),
    };
    *output = current;
    output.extend(old);
    output.extend(new);
    if let Some(expr) = source_index {
        output.push(Projection { expr, alias: None });
    }
    Ok(())
}

fn null_rule_returning_image(width: usize) -> Vec<Projection> {
    (0..width)
        .map(|_| Projection {
            expr: Expr::Literal(Value::Null),
            alias: None,
        })
        .collect()
}

fn rewrite_rule_returning_image(
    returning: &[Projection],
    target_qualifier: &str,
    target_columns: &BTreeSet<String>,
    old_qualifier: &str,
    new_qualifier: &str,
    image_qualifier: &str,
) -> Result<Vec<Projection>, SQLError> {
    let mut resolver = ReturningImageResolver {
        target_qualifier,
        target_columns,
        old_qualifier,
        new_qualifier,
        image_qualifier,
    };
    returning
        .iter()
        .map(|projection| {
            let expr = match &projection.expr {
                Expr::Star => Expr::QualifiedStar(image_qualifier.to_string()),
                Expr::QualifiedStar(qualifier) if resolver.retargets_qualifier(qualifier) => {
                    Expr::QualifiedStar(image_qualifier.to_string())
                }
                expr => crate::engine_events::bind_rule_expr_scoped(
                    expr,
                    &mut resolver,
                    &BTreeSet::new(),
                )?,
            };
            Ok(Projection {
                expr,
                alias: projection.alias.clone(),
            })
        })
        .collect()
}

struct ReturningImageResolver<'a> {
    target_qualifier: &'a str,
    target_columns: &'a BTreeSet<String>,
    old_qualifier: &'a str,
    new_qualifier: &'a str,
    image_qualifier: &'a str,
}

impl ReturningImageResolver<'_> {
    fn retargets_qualifier(&self, qualifier: &str) -> bool {
        qualifier.eq_ignore_ascii_case(self.target_qualifier)
            || qualifier.eq_ignore_ascii_case(self.old_qualifier)
            || qualifier.eq_ignore_ascii_case(self.new_qualifier)
    }
}

impl VariableResolver for ReturningImageResolver<'_> {
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
        Ok(self
            .target_columns
            .contains(name)
            .then(|| Expr::qualified_column(self.image_qualifier, name)))
    }

    fn rewrite_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<Expr>, SQLError> {
        Ok(self
            .retargets_qualifier(qualifier)
            .then(|| Expr::qualified_column(self.image_qualifier, column)))
    }
}
