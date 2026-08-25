//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! AST-independent scalar physical IR shared by the planner and executors.

use uqa_core::{ArrayValue, Value};
use uqa_sql::ast::{BinaryOp, FrameMode, FunctionBinding, NullsOrder};
use uqa_sql::expr::{
    cast_value_with_type_resolution, eval_binary_values, eval_binary_values_with_integer_width,
    eval_builtin_function_call, eval_function_call, integer_width_for_literal,
    integer_width_for_type, negate_value, truthy, EngineHook, EvalContext, IntegerWidth, RowLookup,
    NAMED_ARG_FUNCTION, VARIADIC_ARG_FUNCTION,
};
use uqa_sql::{ResultRow, SQLError, SQLParam};

use crate::batch::{OwnedPhysicalRow, PhysicalRow, RowSchema};

/// Index into the query children owned by the enclosing expression plan.
pub type SubqueryId = usize;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ScalarExpr {
    Star,
    QualifiedStar(String),
    Default,
    Column(String),
    /// Logical position in an already-bound physical row schema. This variant is introduced only after relational binding so duplicate SQL labels remain independently addressable.
    Position(usize),
    QualifiedColumn {
        qualifier: String,
        column: String,
    },
    Literal(Value),
    Param(usize),
    Func {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        binding: Option<FunctionBinding>,
        args: Vec<Self>,
        distinct: bool,
        order_by: Vec<ScalarOrder>,
        filter: Option<Box<Self>>,
    },
    Array(Vec<Self>),
    Row(Vec<Self>),
    Binary {
        op: BinaryOp,
        lhs: Box<Self>,
        rhs: Box<Self>,
    },
    UnaryMinus(Box<Self>),
    Not(Box<Self>),
    And(Vec<Self>),
    Or(Vec<Self>),
    IsNull {
        expr: Box<Self>,
        negated: bool,
    },
    Between {
        expr: Box<Self>,
        low: Box<Self>,
        high: Box<Self>,
    },
    InList {
        expr: Box<Self>,
        list: Vec<Self>,
        negated: bool,
    },
    WindowCall {
        name: String,
        args: Vec<Self>,
        spec: ScalarWindowSpec,
    },
    Case {
        base: Option<Box<Self>>,
        when: Vec<(Self, Self)>,
        else_branch: Option<Box<Self>>,
    },
    Cast {
        expr: Box<Self>,
        ty: String,
    },
    ScalarSubquery(SubqueryId),
    Exists {
        subquery: SubqueryId,
        negated: bool,
    },
    InSubquery {
        expr: Box<Self>,
        subquery: SubqueryId,
        negated: bool,
    },
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ScalarOrder {
    pub expr: ScalarExpr,
    pub descending: bool,
    pub nulls: Option<NullsOrder>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ScalarWindowSpec {
    pub partition_by: Vec<ScalarExpr>,
    pub order_by: Vec<ScalarOrder>,
    pub frame: Option<ScalarWindowFrame>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ScalarWindowFrame {
    pub mode: FrameMode,
    pub start: ScalarFrameBound,
    pub end: ScalarFrameBound,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ScalarFrameBound {
    UnboundedPreceding,
    UnboundedFollowing,
    CurrentRow,
    Preceding(Box<ScalarExpr>),
    Following(Box<ScalarExpr>),
}

impl ScalarExpr {
    #[must_use]
    pub fn qualified_column(qualifier: impl Into<String>, column: impl Into<String>) -> Self {
        Self::QualifiedColumn {
            qualifier: qualifier.into(),
            column: column.into(),
        }
    }

    /// Collect every column needed to evaluate this expression. Returns
    /// `false` when evaluation needs row shape or a relational child that a
    /// projected field scan cannot provide.
    pub fn collect_columns(&self, output: &mut std::collections::BTreeSet<String>) -> bool {
        match self {
            Self::Column(name) | Self::QualifiedColumn { column: name, .. } => {
                output.insert(name.clone());
                true
            }
            Self::Literal(_) | Self::Param(_) => true,
            Self::Func {
                args,
                order_by,
                filter,
                ..
            } => {
                args.iter().all(|arg| arg.collect_columns(output))
                    && order_by
                        .iter()
                        .all(|order| order.expr.collect_columns(output))
                    && filter
                        .as_deref()
                        .is_none_or(|filter| filter.collect_columns(output))
            }
            Self::Array(items) | Self::Row(items) | Self::And(items) | Self::Or(items) => {
                items.iter().all(|item| item.collect_columns(output))
            }
            Self::Binary { lhs, rhs, .. } => {
                lhs.collect_columns(output) && rhs.collect_columns(output)
            }
            Self::UnaryMinus(expr)
            | Self::Not(expr)
            | Self::IsNull { expr, .. }
            | Self::Cast { expr, .. } => expr.collect_columns(output),
            Self::Between { expr, low, high } => {
                expr.collect_columns(output)
                    && low.collect_columns(output)
                    && high.collect_columns(output)
            }
            Self::InList { expr, list, .. } => {
                expr.collect_columns(output) && list.iter().all(|item| item.collect_columns(output))
            }
            Self::Case {
                base,
                when,
                else_branch,
            } => {
                base.as_deref()
                    .is_none_or(|base| base.collect_columns(output))
                    && when.iter().all(|(condition, result)| {
                        condition.collect_columns(output) && result.collect_columns(output)
                    })
                    && else_branch
                        .as_deref()
                        .is_none_or(|branch| branch.collect_columns(output))
            }
            Self::Default
            | Self::Star
            | Self::QualifiedStar(_)
            | Self::Position(_)
            | Self::WindowCall { .. }
            | Self::ScalarSubquery(_)
            | Self::Exists { .. }
            | Self::InSubquery { .. } => false,
        }
    }

    #[must_use]
    pub fn contains_window(&self) -> bool {
        match self {
            Self::WindowCall { .. } => true,
            Self::Func {
                args,
                order_by,
                filter,
                ..
            } => {
                args.iter().any(Self::contains_window)
                    || order_by.iter().any(|order| order.expr.contains_window())
                    || filter.as_deref().is_some_and(Self::contains_window)
            }
            Self::Array(items) | Self::Row(items) | Self::And(items) | Self::Or(items) => {
                items.iter().any(Self::contains_window)
            }
            Self::Binary { lhs, rhs, .. } => lhs.contains_window() || rhs.contains_window(),
            Self::UnaryMinus(expr)
            | Self::Not(expr)
            | Self::IsNull { expr, .. }
            | Self::Cast { expr, .. }
            | Self::InSubquery { expr, .. } => expr.contains_window(),
            Self::Between { expr, low, high } => {
                expr.contains_window() || low.contains_window() || high.contains_window()
            }
            Self::InList { expr, list, .. } => {
                expr.contains_window() || list.iter().any(Self::contains_window)
            }
            Self::Case {
                base,
                when,
                else_branch,
            } => {
                base.as_deref().is_some_and(Self::contains_window)
                    || when.iter().any(|(condition, result)| {
                        condition.contains_window() || result.contains_window()
                    })
                    || else_branch.as_deref().is_some_and(Self::contains_window)
            }
            Self::Default
            | Self::Star
            | Self::QualifiedStar(_)
            | Self::Column(_)
            | Self::QualifiedColumn { .. }
            | Self::Position(_)
            | Self::Literal(_)
            | Self::Param(_)
            | Self::ScalarSubquery(_)
            | Self::Exists { .. } => false,
        }
    }

    #[must_use]
    pub fn contains_subquery(&self) -> bool {
        match self {
            Self::ScalarSubquery(_) | Self::Exists { .. } | Self::InSubquery { .. } => true,
            Self::Func {
                args,
                order_by,
                filter,
                ..
            } => {
                args.iter().any(Self::contains_subquery)
                    || order_by.iter().any(|order| order.expr.contains_subquery())
                    || filter.as_deref().is_some_and(Self::contains_subquery)
            }
            Self::Array(items) | Self::Row(items) | Self::And(items) | Self::Or(items) => {
                items.iter().any(Self::contains_subquery)
            }
            Self::Binary { lhs, rhs, .. } => lhs.contains_subquery() || rhs.contains_subquery(),
            Self::UnaryMinus(expr)
            | Self::Not(expr)
            | Self::IsNull { expr, .. }
            | Self::Cast { expr, .. } => expr.contains_subquery(),
            Self::Between { expr, low, high } => {
                expr.contains_subquery() || low.contains_subquery() || high.contains_subquery()
            }
            Self::InList { expr, list, .. } => {
                expr.contains_subquery() || list.iter().any(Self::contains_subquery)
            }
            Self::WindowCall { args, spec, .. } => {
                args.iter().any(Self::contains_subquery)
                    || spec.partition_by.iter().any(Self::contains_subquery)
                    || spec
                        .order_by
                        .iter()
                        .any(|order| order.expr.contains_subquery())
            }
            Self::Case {
                base,
                when,
                else_branch,
            } => {
                base.as_deref().is_some_and(Self::contains_subquery)
                    || when.iter().any(|(condition, result)| {
                        condition.contains_subquery() || result.contains_subquery()
                    })
                    || else_branch.as_deref().is_some_and(Self::contains_subquery)
            }
            Self::Default
            | Self::Star
            | Self::QualifiedStar(_)
            | Self::Column(_)
            | Self::QualifiedColumn { .. }
            | Self::Position(_)
            | Self::Literal(_)
            | Self::Param(_) => false,
        }
    }

    #[must_use]
    pub fn contains_parameter(&self) -> bool {
        match self {
            Self::Param(_) => true,
            Self::Func {
                args,
                order_by,
                filter,
                ..
            } => {
                args.iter().any(Self::contains_parameter)
                    || order_by.iter().any(|order| order.expr.contains_parameter())
                    || filter.as_deref().is_some_and(Self::contains_parameter)
            }
            Self::Array(items) | Self::Row(items) | Self::And(items) | Self::Or(items) => {
                items.iter().any(Self::contains_parameter)
            }
            Self::Binary { lhs, rhs, .. } => lhs.contains_parameter() || rhs.contains_parameter(),
            Self::UnaryMinus(expr)
            | Self::Not(expr)
            | Self::IsNull { expr, .. }
            | Self::Cast { expr, .. }
            | Self::InSubquery { expr, .. } => expr.contains_parameter(),
            Self::Between { expr, low, high } => {
                expr.contains_parameter() || low.contains_parameter() || high.contains_parameter()
            }
            Self::InList { expr, list, .. } => {
                expr.contains_parameter() || list.iter().any(Self::contains_parameter)
            }
            Self::WindowCall { args, spec, .. } => {
                args.iter().any(Self::contains_parameter)
                    || spec.partition_by.iter().any(Self::contains_parameter)
                    || spec
                        .order_by
                        .iter()
                        .any(|order| order.expr.contains_parameter())
                    || spec.frame.as_ref().is_some_and(|frame| {
                        scalar_frame_bound_contains_parameter(&frame.start)
                            || scalar_frame_bound_contains_parameter(&frame.end)
                    })
            }
            Self::Case {
                base,
                when,
                else_branch,
            } => {
                base.as_deref().is_some_and(Self::contains_parameter)
                    || when.iter().any(|(condition, result)| {
                        condition.contains_parameter() || result.contains_parameter()
                    })
                    || else_branch.as_deref().is_some_and(Self::contains_parameter)
            }
            Self::Default
            | Self::Star
            | Self::QualifiedStar(_)
            | Self::Column(_)
            | Self::QualifiedColumn { .. }
            | Self::Position(_)
            | Self::Literal(_)
            | Self::ScalarSubquery(_)
            | Self::Exists { .. } => false,
        }
    }

    #[must_use]
    pub fn contains_aggregate(&self, is_aggregate: &dyn Fn(&str) -> bool) -> bool {
        match self {
            Self::Func {
                name,
                args,
                order_by,
                filter,
                ..
            } => {
                is_aggregate(name)
                    || args
                        .iter()
                        .any(|expression| expression.contains_aggregate(is_aggregate))
                    || order_by
                        .iter()
                        .any(|order| order.expr.contains_aggregate(is_aggregate))
                    || filter
                        .as_deref()
                        .is_some_and(|expression| expression.contains_aggregate(is_aggregate))
            }
            Self::Array(items) | Self::Row(items) | Self::And(items) | Self::Or(items) => items
                .iter()
                .any(|expression| expression.contains_aggregate(is_aggregate)),
            Self::Binary { lhs, rhs, .. } => {
                lhs.contains_aggregate(is_aggregate) || rhs.contains_aggregate(is_aggregate)
            }
            Self::UnaryMinus(expr)
            | Self::Not(expr)
            | Self::IsNull { expr, .. }
            | Self::Cast { expr, .. }
            | Self::InSubquery { expr, .. } => expr.contains_aggregate(is_aggregate),
            Self::Between { expr, low, high } => {
                expr.contains_aggregate(is_aggregate)
                    || low.contains_aggregate(is_aggregate)
                    || high.contains_aggregate(is_aggregate)
            }
            Self::InList { expr, list, .. } => {
                expr.contains_aggregate(is_aggregate)
                    || list
                        .iter()
                        .any(|item| item.contains_aggregate(is_aggregate))
            }
            Self::Case {
                base,
                when,
                else_branch,
            } => {
                base.as_deref()
                    .is_some_and(|expression| expression.contains_aggregate(is_aggregate))
                    || when.iter().any(|(condition, result)| {
                        condition.contains_aggregate(is_aggregate)
                            || result.contains_aggregate(is_aggregate)
                    })
                    || else_branch
                        .as_deref()
                        .is_some_and(|expression| expression.contains_aggregate(is_aggregate))
            }
            Self::Default
            | Self::Star
            | Self::QualifiedStar(_)
            | Self::Column(_)
            | Self::QualifiedColumn { .. }
            | Self::Position(_)
            | Self::Literal(_)
            | Self::Param(_)
            | Self::ScalarSubquery(_)
            | Self::Exists { .. }
            | Self::WindowCall { .. } => false,
        }
    }
}

fn scalar_frame_bound_contains_parameter(bound: &ScalarFrameBound) -> bool {
    match bound {
        ScalarFrameBound::Preceding(expression) | ScalarFrameBound::Following(expression) => {
            expression.contains_parameter()
        }
        ScalarFrameBound::UnboundedPreceding
        | ScalarFrameBound::UnboundedFollowing
        | ScalarFrameBound::CurrentRow => false,
    }
}

/// Runtime callback for query children referenced by [`SubqueryId`]. The
/// planner owns the actual query-plan arena; execution only needs this stable
/// slot interface.
pub trait ScalarSubqueryRunner {
    fn execute_subquery(
        &self,
        subquery: SubqueryId,
        outer_row: Option<&dyn RowLookup>,
        params: &[SQLParam],
    ) -> Result<SubqueryResult, SQLError>;

    fn execute_subquery_physical(
        &self,
        subquery: SubqueryId,
        outer_schema: &RowSchema,
        outer_row: &PhysicalRow,
        params: &[SQLParam],
    ) -> Result<SubqueryResult, SQLError> {
        let outer = outer_schema.view(outer_row);
        self.execute_subquery(subquery, Some(&outer), params)
    }

    fn scalar_subquery_value(
        &self,
        subquery: SubqueryId,
        outer_row: Option<&dyn RowLookup>,
        params: &[SQLParam],
    ) -> Result<Value, SQLError> {
        self.execute_subquery(subquery, outer_row, params)?
            .into_scalar_value()
    }

    fn scalar_subquery_value_physical(
        &self,
        subquery: SubqueryId,
        outer_schema: &RowSchema,
        outer_row: &PhysicalRow,
        params: &[SQLParam],
    ) -> Result<Value, SQLError> {
        self.execute_subquery_physical(subquery, outer_schema, outer_row, params)?
            .into_scalar_value()
    }

    fn subquery_exists(
        &self,
        subquery: SubqueryId,
        outer_row: Option<&dyn RowLookup>,
        params: &[SQLParam],
    ) -> Result<bool, SQLError> {
        self.execute_subquery(subquery, outer_row, params)?
            .into_exists()
    }

    fn subquery_exists_physical(
        &self,
        subquery: SubqueryId,
        outer_schema: &RowSchema,
        outer_row: &PhysicalRow,
        params: &[SQLParam],
    ) -> Result<bool, SQLError> {
        self.execute_subquery_physical(subquery, outer_schema, outer_row, params)?
            .into_exists()
    }

    fn subquery_contains(
        &self,
        subquery: SubqueryId,
        needle: &Value,
        outer_row: Option<&dyn RowLookup>,
        params: &[SQLParam],
    ) -> Result<Option<bool>, SQLError> {
        self.execute_subquery(subquery, outer_row, params)?
            .contains(needle)
    }

    fn subquery_contains_physical(
        &self,
        subquery: SubqueryId,
        needle: &Value,
        outer_schema: &RowSchema,
        outer_row: &PhysicalRow,
        params: &[SQLParam],
    ) -> Result<Option<bool>, SQLError> {
        self.execute_subquery_physical(subquery, outer_schema, outer_row, params)?
            .contains(needle)
    }
}

/// Pull-based scalar-subquery result. Scalar, EXISTS, and IN consumers never
/// need to materialize the complete child relation: they respectively inspect
/// at most two rows, one row, or one row at a time.
pub struct SubqueryResult {
    pub columns: Vec<String>,
    pub rows: Box<dyn Iterator<Item = Result<OwnedPhysicalRow, SQLError>> + Send>,
}

impl SubqueryResult {
    pub fn from_rows(columns: Vec<String>, rows: Vec<ResultRow>) -> Self {
        let schema = RowSchema::new(columns.clone());
        Self {
            columns,
            rows: Box::new(rows.into_iter().map(move |row| {
                Ok(OwnedPhysicalRow::new(
                    schema.clone(),
                    PhysicalRow::from_result_row(&schema, row),
                ))
            })),
        }
    }

    pub fn into_scalar_value(mut self) -> Result<Value, SQLError> {
        let Some(first_row) = self.rows.next().transpose()? else {
            return Ok(Value::Null);
        };
        if self.rows.next().transpose()?.is_some() {
            return Err(SQLError::TypeMismatch(
                "scalar subquery returned more than one row".into(),
            ));
        }
        if self.columns.is_empty() {
            return Err(SQLError::TypeMismatch(
                "scalar subquery returned no columns".into(),
            ));
        }
        Ok(first_row
            .positional_column(0)
            .cloned()
            .unwrap_or(Value::Null))
    }

    pub fn into_exists(mut self) -> Result<bool, SQLError> {
        Ok(self.rows.next().transpose()?.is_some())
    }

    pub fn contains(self, needle: &Value) -> Result<Option<bool>, SQLError> {
        if self.columns.is_empty() {
            return Ok(Some(false));
        }
        let mut saw_row = false;
        let mut saw_null = false;
        for row in self.rows {
            let row = row?;
            saw_row = true;
            match row.positional_column(0) {
                Some(Value::Null) | None => saw_null = true,
                Some(value) if !matches!(needle, Value::Null) && value == needle => {
                    return Ok(Some(true));
                }
                Some(_) => {}
            }
        }
        Ok(if !saw_row {
            Some(false)
        } else if matches!(needle, Value::Null) || saw_null {
            None
        } else {
            Some(false)
        })
    }
}

pub struct ScalarEvalContext<'a> {
    row: Option<&'a ResultRow>,
    row_lookup: Option<&'a dyn RowLookup>,
    row_schema: Option<&'a RowSchema>,
    params: &'a [SQLParam],
    function_hook: Option<&'a dyn EngineHook>,
    subquery_runner: Option<&'a dyn ScalarSubqueryRunner>,
    physical_outer_row: Option<(&'a RowSchema, &'a PhysicalRow)>,
}

impl<'a> ScalarEvalContext<'a> {
    #[must_use]
    pub fn new(row: Option<&'a ResultRow>, params: &'a [SQLParam]) -> Self {
        Self {
            row,
            row_lookup: row.map(|row| row as &dyn RowLookup),
            row_schema: None,
            params,
            function_hook: None,
            subquery_runner: None,
            physical_outer_row: None,
        }
    }

    #[must_use]
    pub fn from_row_lookup(row: &'a dyn RowLookup, params: &'a [SQLParam]) -> Self {
        Self {
            row: None,
            row_lookup: Some(row),
            row_schema: None,
            params,
            function_hook: None,
            subquery_runner: None,
            physical_outer_row: None,
        }
    }

    #[must_use]
    pub fn with_function_hook(mut self, hook: &'a dyn EngineHook) -> Self {
        self.function_hook = Some(hook);
        self
    }

    #[must_use]
    pub fn with_row_schema(mut self, schema: &'a RowSchema) -> Self {
        self.row_schema = Some(schema);
        self
    }

    #[must_use]
    pub fn with_subquery_runner(mut self, runner: &'a dyn ScalarSubqueryRunner) -> Self {
        self.subquery_runner = Some(runner);
        self
    }

    #[must_use]
    pub fn with_physical_outer_row(mut self, schema: &'a RowSchema, row: &'a PhysicalRow) -> Self {
        self.physical_outer_row = Some((schema, row));
        self
    }

    fn sql_context(&self) -> EvalContext<'_> {
        let context = self.row_lookup.map_or_else(
            || EvalContext::new(self.row, self.params),
            |row| EvalContext::from_row_lookup(row, self.params),
        );
        match self.function_hook {
            Some(hook) => context.with_engine(hook),
            None => context,
        }
    }

    fn outer_row(&self) -> Option<&dyn RowLookup> {
        self.row_lookup
    }
}

/// Evaluate the physical scalar tree directly. No parser expression is
/// reconstructed at this boundary.
pub fn eval_scalar(
    expression: &ScalarExpr,
    context: &ScalarEvalContext<'_>,
) -> Result<Value, SQLError> {
    match expression {
        ScalarExpr::Default => Err(SQLError::Internal(
            "DEFAULT reached scalar expression evaluation without a mutation target".into(),
        )),
        ScalarExpr::Star | ScalarExpr::QualifiedStar(_) => {
            Err(SQLError::Internal("`*` cannot be evaluated".into()))
        }
        ScalarExpr::Column(name) => context.sql_context().column_value(name),
        ScalarExpr::Position(position) => context
            .row_lookup
            .and_then(|row| row.positional_column(*position))
            .cloned()
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "bound physical column position {position} is unavailable"
                ))
            }),
        ScalarExpr::QualifiedColumn { qualifier, column } => context
            .sql_context()
            .qualified_column_value(qualifier, column),
        ScalarExpr::Literal(value) => Ok(value.clone()),
        ScalarExpr::Param(index) => eval_parameter(*index, context.params),
        ScalarExpr::Func {
            name,
            binding,
            args,
            ..
        } => {
            let arguments = eval_call_arguments(args, context)?;
            if let Some(binding) = binding {
                if binding.builtin {
                    let dispatch_name = crate::type_resolution::runtime_dispatch_name(binding);
                    return eval_builtin_function_call(
                        dispatch_name.as_deref().unwrap_or(&binding.name),
                        arguments,
                        &context.sql_context(),
                    );
                }
                let sql_context = context.sql_context();
                let engine = sql_context.engine.ok_or_else(|| {
                    SQLError::Unsupported(
                        "bound user function requires a logical engine session".into(),
                    )
                })?;
                engine
                    .call_bound_user_function(binding, &arguments)
                    .unwrap_or_else(|| Err(SQLError::UnknownFunction(binding.name.clone())))
            } else {
                eval_function_call(name, arguments, &context.sql_context())
            }
        }
        ScalarExpr::Array(items) => items
            .iter()
            .map(|item| eval_scalar(item, context))
            .collect::<Result<Vec<_>, _>>()
            .and_then(|items| {
                ArrayValue::try_new(items).map(Value::Array).ok_or_else(|| {
                    SQLError::TypeMismatch(
                        "multidimensional arrays must have matching dimensions".into(),
                    )
                })
            }),
        ScalarExpr::Row(items) => items
            .iter()
            .map(|item| eval_scalar(item, context))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Row),
        ScalarExpr::Binary { op, lhs, rhs } => {
            let left = eval_scalar(lhs, context)?;
            let right = eval_scalar(rhs, context)?;
            eval_binary_values_with_integer_width(
                *op,
                &left,
                &right,
                scalar_integer_binary_width(lhs, rhs),
            )
        }
        ScalarExpr::UnaryMinus(inner) => {
            let source_ty = scalar_source_type(inner, context);
            let value = eval_scalar(inner, context)?;
            negate_value(&value, source_ty.as_deref())
        }
        ScalarExpr::Not(inner) => {
            let value = eval_scalar(inner, context)?;
            if matches!(value, Value::Null) {
                Ok(Value::Null)
            } else {
                Ok(Value::Bool(!truthy(&value)))
            }
        }
        ScalarExpr::And(items) => eval_and(items, context),
        ScalarExpr::Or(items) => eval_or(items, context),
        ScalarExpr::IsNull { expr, negated } => {
            let is_null = matches!(eval_scalar(expr, context)?, Value::Null);
            Ok(Value::Bool(if *negated { !is_null } else { is_null }))
        }
        ScalarExpr::Between { expr, low, high } => eval_between(expr, low, high, context),
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => eval_in_list(expr, list, *negated, context),
        ScalarExpr::WindowCall { name, .. } => Err(SQLError::Unsupported(format!(
            "window function `{name}` must be evaluated by the window-aware executor"
        ))),
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => eval_case(base.as_deref(), when, else_branch.as_deref(), context),
        ScalarExpr::Cast { expr, ty } => {
            let source_ty = scalar_source_type(expr, context);
            let value = eval_scalar(expr, context)?;
            cast_value_with_type_resolution(&value, source_ty.as_deref(), ty, context.function_hook)
        }
        ScalarExpr::ScalarSubquery(subquery) => execute_scalar_subquery(*subquery, context),
        ScalarExpr::Exists { subquery, negated } => {
            let exists = execute_exists_subquery(*subquery, context)?;
            Ok(Value::Bool(if *negated { !exists } else { exists }))
        }
        ScalarExpr::InSubquery {
            expr,
            subquery,
            negated,
        } => {
            let needle = eval_scalar(expr, context)?;
            let found = execute_in_subquery(*subquery, &needle, context)?;
            Ok(found.map_or(Value::Null, |found| {
                Value::Bool(if *negated { !found } else { found })
            }))
        }
    }
}

fn eval_parameter(index: usize, params: &[SQLParam]) -> Result<Value, SQLError> {
    match index
        .checked_sub(1)
        .and_then(|parameter_index| params.get(parameter_index))
    {
        Some(SQLParam::Scalar(value) | SQLParam::TypedScalar { value, .. }) => Ok(value.clone()),
        Some(SQLParam::Vector(vector)) => Ok(Value::List(
            vector
                .iter()
                .map(|value| Value::Float(f64::from(*value)))
                .collect(),
        )),
        Some(SQLParam::Tensor(vectors)) => Ok(Value::List(
            vectors
                .iter()
                .map(|vector| {
                    Value::List(
                        vector
                            .iter()
                            .map(|value| Value::Float(f64::from(*value)))
                            .collect(),
                    )
                })
                .collect(),
        )),
        None => Err(SQLError::MissingParam(index)),
    }
}

pub fn eval_call_arguments(
    arguments: &[ScalarExpr],
    context: &ScalarEvalContext<'_>,
) -> Result<Vec<(Option<String>, Value)>, SQLError> {
    scalar_call_arguments(arguments)?
        .into_iter()
        .map(|argument| {
            Ok((
                argument.name.map(str::to_string),
                eval_scalar(argument.value, context)?,
            ))
        })
        .collect()
}

/// A physical call argument after removing the compiler's named and explicit `VARIADIC` syntax markers.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScalarCallArgument<'a> {
    pub name: Option<&'a str>,
    pub value: &'a ScalarExpr,
    pub explicit_variadic: bool,
}

/// Decode and validate all compiler-owned call-argument markers. PostgreSQL permits one explicit `VARIADIC` argument and requires it to be the final argument.
#[doc(hidden)]
pub fn scalar_call_arguments(
    arguments: &[ScalarExpr],
) -> Result<Vec<ScalarCallArgument<'_>>, SQLError> {
    let mut decoded = Vec::with_capacity(arguments.len());
    for argument in arguments {
        decoded.push(scalar_call_argument(argument)?);
    }
    validate_scalar_call_arguments(&decoded)?;
    Ok(decoded)
}

/// Validate cross-argument invariants after individual syntax markers have been decoded, returning whether the call used explicit `VARIADIC` syntax.
#[doc(hidden)]
pub fn validate_scalar_call_arguments(
    arguments: &[ScalarCallArgument<'_>],
) -> Result<bool, SQLError> {
    let variadic_positions = arguments
        .iter()
        .enumerate()
        .filter_map(|(position, argument)| argument.explicit_variadic.then_some(position))
        .collect::<Vec<_>>();
    if variadic_positions.len() > 1 {
        return Err(malformed_call_argument(
            "call contains more than one explicit VARIADIC argument",
        ));
    }
    if variadic_positions
        .first()
        .is_some_and(|position| position + 1 != arguments.len())
    {
        return Err(malformed_call_argument(
            "explicit VARIADIC argument must be the final call argument",
        ));
    }
    Ok(!variadic_positions.is_empty())
}

/// Decode one compiler-owned call-argument marker. Use [`scalar_call_arguments`] for a complete call so duplicate and ordering invariants are also checked.
#[doc(hidden)]
pub fn scalar_call_argument(expression: &ScalarExpr) -> Result<ScalarCallArgument<'_>, SQLError> {
    let ScalarExpr::Func {
        name,
        args,
        binding,
        distinct,
        order_by,
        filter,
    } = expression
    else {
        return Ok(ScalarCallArgument {
            name: None,
            value: expression,
            explicit_variadic: false,
        });
    };
    if name == NAMED_ARG_FUNCTION {
        validate_marker_shape(
            binding.as_ref(),
            *distinct,
            order_by,
            filter.as_deref(),
            name,
        )?;
        let [ScalarExpr::Literal(Value::Str(argument_name)), value] = args.as_slice() else {
            return Err(malformed_call_argument(
                "named argument marker must contain a string name and one value",
            ));
        };
        let (value, explicit_variadic) = direct_variadic_argument(value)?;
        if !explicit_variadic
            && matches!(
                value,
                ScalarExpr::Func { name, .. } if name == NAMED_ARG_FUNCTION
            )
        {
            return Err(malformed_call_argument(
                "call argument contains nested syntax markers",
            ));
        }
        return Ok(ScalarCallArgument {
            name: Some(argument_name),
            value,
            explicit_variadic,
        });
    }
    let (value, explicit_variadic) = direct_variadic_argument(expression)?;
    Ok(ScalarCallArgument {
        name: None,
        value,
        explicit_variadic,
    })
}

fn direct_variadic_argument(expression: &ScalarExpr) -> Result<(&ScalarExpr, bool), SQLError> {
    let ScalarExpr::Func {
        name,
        args,
        binding,
        distinct,
        order_by,
        filter,
    } = expression
    else {
        return Ok((expression, false));
    };
    if name != VARIADIC_ARG_FUNCTION {
        return Ok((expression, false));
    }
    validate_marker_shape(
        binding.as_ref(),
        *distinct,
        order_by,
        filter.as_deref(),
        name,
    )?;
    let [value] = args.as_slice() else {
        return Err(malformed_call_argument(
            "VARIADIC argument marker must contain exactly one value",
        ));
    };
    if matches!(
        value,
        ScalarExpr::Func { name, .. }
            if name == VARIADIC_ARG_FUNCTION || name == NAMED_ARG_FUNCTION
    ) {
        return Err(malformed_call_argument(
            "call argument contains nested syntax markers",
        ));
    }
    Ok((value, true))
}

fn validate_marker_shape(
    binding: Option<&FunctionBinding>,
    distinct: bool,
    order_by: &[ScalarOrder],
    filter: Option<&ScalarExpr>,
    name: &str,
) -> Result<(), SQLError> {
    if binding.is_some() || distinct || !order_by.is_empty() || filter.is_some() {
        return Err(malformed_call_argument(&format!(
            "{name} syntax marker contains function-call metadata"
        )));
    }
    Ok(())
}

fn malformed_call_argument(message: &str) -> SQLError {
    SQLError::Internal(format!("malformed call argument: {message}"))
}

fn eval_and(items: &[ScalarExpr], context: &ScalarEvalContext<'_>) -> Result<Value, SQLError> {
    let mut saw_null = false;
    for item in items {
        let value = eval_scalar(item, context)?;
        if matches!(value, Value::Null) {
            saw_null = true;
        } else if !truthy(&value) {
            return Ok(Value::Bool(false));
        }
    }
    Ok(if saw_null {
        Value::Null
    } else {
        Value::Bool(true)
    })
}

fn eval_or(items: &[ScalarExpr], context: &ScalarEvalContext<'_>) -> Result<Value, SQLError> {
    let mut saw_null = false;
    for item in items {
        let value = eval_scalar(item, context)?;
        if matches!(value, Value::Null) {
            saw_null = true;
        } else if truthy(&value) {
            return Ok(Value::Bool(true));
        }
    }
    Ok(if saw_null {
        Value::Null
    } else {
        Value::Bool(false)
    })
}

fn eval_between(
    expression: &ScalarExpr,
    low: &ScalarExpr,
    high: &ScalarExpr,
    context: &ScalarEvalContext<'_>,
) -> Result<Value, SQLError> {
    let value = eval_scalar(expression, context)?;
    let low = eval_scalar(low, context)?;
    let high = eval_scalar(high, context)?;
    let greater_equal = eval_binary_values(BinaryOp::GreaterEqual, &value, &low)?;
    let less_equal = eval_binary_values(BinaryOp::LessEqual, &value, &high)?;
    match (greater_equal, less_equal) {
        (Value::Bool(false), _) | (_, Value::Bool(false)) => Ok(Value::Bool(false)),
        (Value::Bool(true), Value::Bool(true)) => Ok(Value::Bool(true)),
        _ => Ok(Value::Null),
    }
}

fn eval_in_list(
    expression: &ScalarExpr,
    list: &[ScalarExpr],
    negated: bool,
    context: &ScalarEvalContext<'_>,
) -> Result<Value, SQLError> {
    let needle = eval_scalar(expression, context)?;
    let mut saw_null = matches!(needle, Value::Null);
    for item in list {
        let candidate = eval_scalar(item, context)?;
        match eval_binary_values(BinaryOp::Equal, &needle, &candidate)? {
            Value::Bool(true) => return Ok(Value::Bool(!negated)),
            Value::Null => saw_null = true,
            _ => {}
        }
    }
    Ok(if saw_null {
        Value::Null
    } else {
        Value::Bool(negated)
    })
}

fn eval_case(
    base: Option<&ScalarExpr>,
    branches: &[(ScalarExpr, ScalarExpr)],
    else_branch: Option<&ScalarExpr>,
    context: &ScalarEvalContext<'_>,
) -> Result<Value, SQLError> {
    let base = base
        .map(|expression| eval_scalar(expression, context))
        .transpose()?;
    for (condition, result) in branches {
        let condition = eval_scalar(condition, context)?;
        let matched = match &base {
            Some(base) => matches!(
                eval_binary_values(BinaryOp::Equal, base, &condition)?,
                Value::Bool(true)
            ),
            None => truthy(&condition),
        };
        if matched {
            return eval_scalar(result, context);
        }
    }
    else_branch.map_or(Ok(Value::Null), |expression| {
        eval_scalar(expression, context)
    })
}

fn execute_scalar_subquery(
    subquery: SubqueryId,
    context: &ScalarEvalContext<'_>,
) -> Result<Value, SQLError> {
    let runner = context
        .subquery_runner
        .ok_or_else(|| SQLError::Unsupported("physical subquery requires a plan runner".into()))?;
    match context.physical_outer_row {
        Some((schema, row)) => {
            runner.scalar_subquery_value_physical(subquery, schema, row, context.params)
        }
        None => runner.scalar_subquery_value(subquery, context.outer_row(), context.params),
    }
}

fn execute_exists_subquery(
    subquery: SubqueryId,
    context: &ScalarEvalContext<'_>,
) -> Result<bool, SQLError> {
    let runner = context
        .subquery_runner
        .ok_or_else(|| SQLError::Unsupported("physical subquery requires a plan runner".into()))?;
    match context.physical_outer_row {
        Some((schema, row)) => {
            runner.subquery_exists_physical(subquery, schema, row, context.params)
        }
        None => runner.subquery_exists(subquery, context.outer_row(), context.params),
    }
}

fn execute_in_subquery(
    subquery: SubqueryId,
    needle: &Value,
    context: &ScalarEvalContext<'_>,
) -> Result<Option<bool>, SQLError> {
    let runner = context
        .subquery_runner
        .ok_or_else(|| SQLError::Unsupported("physical subquery requires a plan runner".into()))?;
    match context.physical_outer_row {
        Some((schema, row)) => {
            runner.subquery_contains_physical(subquery, needle, schema, row, context.params)
        }
        None => runner.subquery_contains(subquery, needle, context.outer_row(), context.params),
    }
}

fn scalar_source_type(expression: &ScalarExpr, context: &ScalarEvalContext<'_>) -> Option<String> {
    match expression {
        ScalarExpr::Cast { ty, .. } => return Some(ty.clone()),
        ScalarExpr::UnaryMinus(inner) => return scalar_source_type(inner, context),
        ScalarExpr::Literal(Value::Int(value)) if i32::try_from(*value).is_ok() => {
            return Some("integer".into());
        }
        ScalarExpr::Literal(Value::Int(_)) => return Some("bigint".into()),
        ScalarExpr::Literal(Value::Bytes(_)) => return Some("bytea".into()),
        ScalarExpr::Literal(Value::Str(_) | Value::FixedChar(_)) => return None,
        _ => {}
    }
    context
        .row_schema
        .and_then(|schema| {
            crate::scalar_type(expression, schema, context.params)
                .ok()
                .flatten()
        })
        .map(|ty| ty.sql_name())
}

fn scalar_integer_width(expression: &ScalarExpr) -> Option<IntegerWidth> {
    match expression {
        ScalarExpr::Literal(Value::Int(value)) => Some(integer_width_for_literal(*value)),
        ScalarExpr::Cast { ty, .. } => integer_width_for_type(ty),
        ScalarExpr::UnaryMinus(inner) => scalar_integer_width(inner),
        ScalarExpr::Binary {
            op: BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide,
            lhs,
            rhs,
        } => Some(scalar_integer_width(lhs)?.max(scalar_integer_width(rhs)?)),
        _ => None,
    }
}

pub(crate) fn scalar_integer_binary_width(
    lhs: &ScalarExpr,
    rhs: &ScalarExpr,
) -> Option<IntegerWidth> {
    Some(scalar_integer_width(lhs)?.max(scalar_integer_width(rhs)?))
}

#[cfg(test)]
mod tests {
    use super::{
        eval_call_arguments, eval_scalar, scalar_call_arguments, ScalarEvalContext, ScalarExpr,
    };
    use crate::{PhysicalRow, RowSchema};
    use uqa_core::Value;
    use uqa_sql::ast::{BinaryOp, ColumnType};
    use uqa_sql::{SQLError, SQLParam};

    #[test]
    fn arithmetic_does_not_require_parser_ast() {
        let expression = ScalarExpr::Binary {
            op: BinaryOp::Multiply,
            lhs: Box::new(ScalarExpr::Literal(Value::Int(7))),
            rhs: Box::new(ScalarExpr::Literal(Value::Int(3))),
        };
        assert_eq!(
            eval_scalar(&expression, &ScalarEvalContext::new(None, &[])).unwrap(),
            Value::Int(21)
        );
    }

    #[test]
    fn cast_uses_the_input_schema_declared_source_type() {
        let schema = RowSchema::with_types(vec!["support".into()], vec![Some(ColumnType::Regproc)]);
        let row = PhysicalRow::from_values(vec![Value::Int(0)]);
        let view = schema.view(&row);
        let expression = ScalarExpr::Cast {
            expr: Box::new(ScalarExpr::Column("support".into())),
            ty: "text".into(),
        };
        assert_eq!(
            eval_scalar(
                &expression,
                &ScalarEvalContext::from_row_lookup(&view, &[]).with_row_schema(&schema),
            )
            .unwrap(),
            Value::Str("-".into())
        );
    }

    #[test]
    fn cast_preserves_unknown_type_for_string_literals() {
        let expression = ScalarExpr::Cast {
            expr: Box::new(ScalarExpr::Literal(Value::Str("[1,5)".into()))),
            ty: "int4range".into(),
        };
        assert_eq!(
            eval_scalar(&expression, &ScalarEvalContext::new(None, &[])).unwrap(),
            Value::Str("[1,5)".into())
        );
    }

    #[test]
    fn parameter_zero_is_not_aliased_to_parameter_one() {
        let params = [SQLParam::Scalar(Value::Str("secret".into()))];
        assert!(matches!(
            eval_scalar(
                &ScalarExpr::Param(0),
                &ScalarEvalContext::new(None, &params)
            ),
            Err(SQLError::MissingParam(0))
        ));
    }

    #[test]
    fn typed_scalar_parameter_evaluates_like_scalar() {
        let params = [SQLParam::typed_scalar(
            Value::Int(7),
            ColumnType::SmallInteger,
        )];
        assert_eq!(
            eval_scalar(
                &ScalarExpr::Param(1),
                &ScalarEvalContext::new(None, &params)
            )
            .unwrap(),
            Value::Int(7)
        );
    }

    #[test]
    fn parameter_detection_descends_into_nested_expressions() {
        let expression = ScalarExpr::Func {
            name: "knn_match".into(),
            binding: None,
            args: vec![
                ScalarExpr::Column("embedding".into()),
                ScalarExpr::Array(vec![ScalarExpr::Param(1)]),
                ScalarExpr::Literal(Value::Int(3)),
            ],
            distinct: false,
            order_by: Vec::new(),
            filter: None,
        };

        assert!(expression.contains_parameter());
        assert!(!ScalarExpr::Literal(Value::Int(3)).contains_parameter());
    }

    #[test]
    fn explicit_variadic_call_argument_is_transparent_to_runtime_evaluation() {
        let arguments = vec![marker(
            uqa_sql::expr::NAMED_ARG_FUNCTION,
            vec![
                ScalarExpr::Literal(Value::Str("items".into())),
                marker(
                    uqa_sql::expr::VARIADIC_ARG_FUNCTION,
                    vec![ScalarExpr::Literal(Value::Int(42))],
                ),
            ],
        )];

        let decoded = scalar_call_arguments(&arguments).unwrap();
        assert_eq!(decoded[0].name, Some("items"));
        assert!(decoded[0].explicit_variadic);
        assert_eq!(decoded[0].value, &ScalarExpr::Literal(Value::Int(42)));
        assert_eq!(
            eval_call_arguments(&arguments, &ScalarEvalContext::new(None, &[])).unwrap(),
            vec![(Some("items".into()), Value::Int(42))]
        );
    }

    #[test]
    fn call_argument_markers_reject_duplicates_and_malformed_nesting() {
        let duplicate = vec![
            marker(
                uqa_sql::expr::VARIADIC_ARG_FUNCTION,
                vec![ScalarExpr::Literal(Value::Int(1))],
            ),
            marker(
                uqa_sql::expr::VARIADIC_ARG_FUNCTION,
                vec![ScalarExpr::Literal(Value::Int(2))],
            ),
        ];
        assert!(matches!(
            scalar_call_arguments(&duplicate),
            Err(SQLError::Internal(message)) if message.contains("more than one")
        ));

        let nested = vec![marker(
            uqa_sql::expr::VARIADIC_ARG_FUNCTION,
            vec![marker(
                uqa_sql::expr::VARIADIC_ARG_FUNCTION,
                vec![ScalarExpr::Literal(Value::Int(1))],
            )],
        )];
        assert!(matches!(
            scalar_call_arguments(&nested),
            Err(SQLError::Internal(message)) if message.contains("nested")
        ));
    }

    fn marker(name: &str, args: Vec<ScalarExpr>) -> ScalarExpr {
        ScalarExpr::Func {
            name: name.into(),
            binding: None,
            args,
            distinct: false,
            order_by: Vec::new(),
            filter: None,
        }
    }
}
