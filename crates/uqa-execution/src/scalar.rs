//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! AST-independent scalar physical IR shared by the planner and executors.

use uqa_core::Value;
use uqa_sql::ast::{BinaryOp, FrameMode, NullsOrder};
use uqa_sql::expr::{
    cast_value, eval_binary_values, eval_function_call, truthy, EngineHook, EvalContext, RowLookup,
    NAMED_ARG_FUNCTION,
};
use uqa_sql::{ResultRow, SQLError, SQLParam};

/// Index into the query children owned by the enclosing expression plan.
pub type SubqueryId = usize;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ScalarExpr {
    Star,
    Column(String),
    QualifiedColumn {
        qualifier: String,
        column: String,
        key: String,
    },
    Literal(Value),
    Param(usize),
    Func {
        name: String,
        args: Vec<Self>,
        distinct: bool,
        order_by: Vec<ScalarOrder>,
        filter: Option<Box<Self>>,
    },
    Array(Vec<Self>),
    Binary {
        op: BinaryOp,
        lhs: Box<Self>,
        rhs: Box<Self>,
    },
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
        let qualifier = qualifier.into();
        let column = column.into();
        let key = format!("{qualifier}.{column}");
        Self::QualifiedColumn {
            qualifier,
            column,
            key,
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
            Self::Array(items) | Self::And(items) | Self::Or(items) => {
                items.iter().all(|item| item.collect_columns(output))
            }
            Self::Binary { lhs, rhs, .. } => {
                lhs.collect_columns(output) && rhs.collect_columns(output)
            }
            Self::Not(expr) | Self::IsNull { expr, .. } | Self::Cast { expr, .. } => {
                expr.collect_columns(output)
            }
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
            Self::Star
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
            Self::Array(items) | Self::And(items) | Self::Or(items) => {
                items.iter().any(Self::contains_window)
            }
            Self::Binary { lhs, rhs, .. } => lhs.contains_window() || rhs.contains_window(),
            Self::Not(expr) | Self::IsNull { expr, .. } | Self::Cast { expr, .. } => {
                expr.contains_window()
            }
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
            Self::InSubquery { expr, .. } => expr.contains_window(),
            Self::Star
            | Self::Column(_)
            | Self::QualifiedColumn { .. }
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
            Self::Array(items) | Self::And(items) | Self::Or(items) => {
                items.iter().any(Self::contains_subquery)
            }
            Self::Binary { lhs, rhs, .. } => lhs.contains_subquery() || rhs.contains_subquery(),
            Self::Not(expr) | Self::IsNull { expr, .. } | Self::Cast { expr, .. } => {
                expr.contains_subquery()
            }
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
            Self::Star
            | Self::Column(_)
            | Self::QualifiedColumn { .. }
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
            Self::Array(items) | Self::And(items) | Self::Or(items) => {
                items.iter().any(Self::contains_parameter)
            }
            Self::Binary { lhs, rhs, .. } => lhs.contains_parameter() || rhs.contains_parameter(),
            Self::Not(expr) | Self::IsNull { expr, .. } | Self::Cast { expr, .. } => {
                expr.contains_parameter()
            }
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
            Self::InSubquery { expr, .. } => expr.contains_parameter(),
            Self::Star
            | Self::Column(_)
            | Self::QualifiedColumn { .. }
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
            Self::Array(items) | Self::And(items) | Self::Or(items) => items
                .iter()
                .any(|expression| expression.contains_aggregate(is_aggregate)),
            Self::Binary { lhs, rhs, .. } => {
                lhs.contains_aggregate(is_aggregate) || rhs.contains_aggregate(is_aggregate)
            }
            Self::Not(expr) | Self::IsNull { expr, .. } | Self::Cast { expr, .. } => {
                expr.contains_aggregate(is_aggregate)
            }
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
            Self::InSubquery { expr, .. } => expr.contains_aggregate(is_aggregate),
            Self::Star
            | Self::Column(_)
            | Self::QualifiedColumn { .. }
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

    fn scalar_subquery_value(
        &self,
        subquery: SubqueryId,
        outer_row: Option<&dyn RowLookup>,
        params: &[SQLParam],
    ) -> Result<Value, SQLError> {
        self.execute_subquery(subquery, outer_row, params)?
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
}

/// Pull-based scalar-subquery result. Scalar, EXISTS, and IN consumers never
/// need to materialize the complete child relation: they respectively inspect
/// at most two rows, one row, or one row at a time.
pub struct SubqueryResult {
    pub columns: Vec<String>,
    pub rows: Box<dyn Iterator<Item = Result<ResultRow, SQLError>> + Send>,
}

impl SubqueryResult {
    pub fn from_rows(columns: Vec<String>, rows: Vec<ResultRow>) -> Self {
        Self {
            columns,
            rows: Box::new(rows.into_iter().map(Ok)),
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
        let first_column = self
            .columns
            .first()
            .ok_or_else(|| SQLError::TypeMismatch("scalar subquery returned no columns".into()))?;
        Ok(first_row.get(first_column).cloned().unwrap_or(Value::Null))
    }

    pub fn into_exists(mut self) -> Result<bool, SQLError> {
        Ok(self.rows.next().transpose()?.is_some())
    }

    pub fn contains(self, needle: &Value) -> Result<Option<bool>, SQLError> {
        let Some(first_column) = self.columns.first() else {
            return Ok(Some(false));
        };
        let mut saw_row = false;
        let mut saw_null = false;
        for row in self.rows {
            let row = row?;
            saw_row = true;
            match row.get(first_column) {
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
    params: &'a [SQLParam],
    function_hook: Option<&'a dyn EngineHook>,
    subquery_runner: Option<&'a dyn ScalarSubqueryRunner>,
}

impl<'a> ScalarEvalContext<'a> {
    #[must_use]
    pub fn new(row: Option<&'a ResultRow>, params: &'a [SQLParam]) -> Self {
        Self {
            row,
            row_lookup: row.map(|row| row as &dyn RowLookup),
            params,
            function_hook: None,
            subquery_runner: None,
        }
    }

    #[must_use]
    pub fn from_row_lookup(row: &'a dyn RowLookup, params: &'a [SQLParam]) -> Self {
        Self {
            row: None,
            row_lookup: Some(row),
            params,
            function_hook: None,
            subquery_runner: None,
        }
    }

    #[must_use]
    pub fn with_function_hook(mut self, hook: &'a dyn EngineHook) -> Self {
        self.function_hook = Some(hook);
        self
    }

    #[must_use]
    pub fn with_subquery_runner(mut self, runner: &'a dyn ScalarSubqueryRunner) -> Self {
        self.subquery_runner = Some(runner);
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
        ScalarExpr::Star => Err(SQLError::Internal("`*` cannot be evaluated".into())),
        ScalarExpr::Column(name) => context.sql_context().column_value(name),
        ScalarExpr::QualifiedColumn {
            qualifier,
            column,
            key,
        } => context
            .sql_context()
            .qualified_column_value(qualifier, column, key),
        ScalarExpr::Literal(value) => Ok(value.clone()),
        ScalarExpr::Param(index) => eval_parameter(*index, context.params),
        ScalarExpr::Func { name, args, .. } => {
            let arguments = eval_call_arguments(args, context)?;
            eval_function_call(name, arguments, &context.sql_context())
        }
        ScalarExpr::Array(items) => items
            .iter()
            .map(|item| eval_scalar(item, context))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::List),
        ScalarExpr::Binary { op, lhs, rhs } => {
            let left = eval_scalar(lhs, context)?;
            let right = eval_scalar(rhs, context)?;
            eval_binary_values(*op, &left, &right)
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
            let value = eval_scalar(expr, context)?;
            cast_value(&value, ty)
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
        Some(SQLParam::Scalar(value)) => Ok(value.clone()),
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
    arguments
        .iter()
        .map(|argument| match argument {
            ScalarExpr::Func {
                name,
                args: marker_args,
                ..
            } if name == NAMED_ARG_FUNCTION => {
                let Some(ScalarExpr::Literal(Value::Str(argument_name))) = marker_args.first()
                else {
                    return Err(SQLError::Internal("named argument without a name".into()));
                };
                let value = marker_args
                    .get(1)
                    .ok_or_else(|| SQLError::Internal("named argument without a value".into()))?;
                Ok((
                    Some(argument_name.to_ascii_lowercase()),
                    eval_scalar(value, context)?,
                ))
            }
            other => Ok((None, eval_scalar(other, context)?)),
        })
        .collect()
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
    runner.scalar_subquery_value(subquery, context.outer_row(), context.params)
}

fn execute_exists_subquery(
    subquery: SubqueryId,
    context: &ScalarEvalContext<'_>,
) -> Result<bool, SQLError> {
    let runner = context
        .subquery_runner
        .ok_or_else(|| SQLError::Unsupported("physical subquery requires a plan runner".into()))?;
    runner.subquery_exists(subquery, context.outer_row(), context.params)
}

fn execute_in_subquery(
    subquery: SubqueryId,
    needle: &Value,
    context: &ScalarEvalContext<'_>,
) -> Result<Option<bool>, SQLError> {
    let runner = context
        .subquery_runner
        .ok_or_else(|| SQLError::Unsupported("physical subquery requires a plan runner".into()))?;
    runner.subquery_contains(subquery, needle, context.outer_row(), context.params)
}

#[cfg(test)]
mod tests {
    use super::{eval_scalar, ScalarEvalContext, ScalarExpr};
    use uqa_core::Value;
    use uqa_sql::ast::BinaryOp;
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
    fn parameter_detection_descends_into_nested_expressions() {
        let expression = ScalarExpr::Func {
            name: "knn_match".into(),
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
}
