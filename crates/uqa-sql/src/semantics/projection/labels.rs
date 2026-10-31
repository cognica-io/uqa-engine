//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed output labels and admitted identifier decoding share the ordinary naming rules.

use crate::{
    ast::{FunctionBinding, FunctionDispatch},
    plan::{ProjectionPlan, QueryPlan, RelationalPlan},
    SQLError, ScalarExpr,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

enum Label<'a> {
    Borrowed(&'a str),
    Decoded(Produced<Vec<String>>),
}

impl Label<'_> {
    fn as_str(&self) -> &str {
        match self {
            Self::Borrowed(name) => name,
            Self::Decoded(names) => names.last().expect("parsed identifier"),
        }
    }

    fn into_uncontrolled(self) -> String {
        match self {
            Self::Borrowed(name) => name.to_owned(),
            Self::Decoded(names) => names
                .into_uncontrolled()
                .expect("ordinary label has no reservation")
                .pop()
                .expect("parsed identifier"),
        }
    }
}

/// An immutable positional descriptor borrowing existing names and retaining admitted decoding storage only where naming requires it.
pub struct ProjectionLabels<'a> {
    labels: Produced<Vec<Label<'a>>>,
}

impl ProjectionLabels<'_> {
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &str> {
        self.labels.iter().map(Label::as_str)
    }
}

/// Capture labels for repeated observation. Aliases override individual projections; duplicate labels retain their original positions. Admit descriptor capacity before decoding any name.
pub fn projection_labels_with_control<'a>(
    projections: &'a [ProjectionPlan],
    aliases: &'a [String],
    control: &ProductionControl<'_>,
) -> Result<ProjectionLabels<'a>, SQLError> {
    let mut labels = ProductionVec::new(*control);
    labels.reserve(projections.len())?;
    for (index, projection) in projections.iter().enumerate() {
        control.check()?;
        let label = if let Some(alias) = aliases.get(index) {
            Label::Borrowed(alias)
        } else {
            projection_label(projection, control)?
        };
        labels.push_produced(control.finish(label, control.empty_reservation())?)?;
    }
    Ok(ProjectionLabels {
        labels: labels.finish()?,
    })
}

/// Compute a projection's `PostgreSQL` output column name. Standalone expressions use `?column?`; repeated labels remain repeated until the final named-map compatibility boundary.
pub fn projection_label_at(projection: &ProjectionPlan) -> String {
    projection_label(projection, &ProductionControl::uncontrolled())
        .expect("ordinary label observation cannot be cancelled or limited")
        .into_uncontrolled()
}

/// Lend one projection label through an observation, admitting any decoding workspace and retaining it until the callback returns.
pub fn with_projection_label<T>(
    projection: &ProjectionPlan,
    control: &ProductionControl<'_>,
    observe: impl FnOnce(&str) -> Result<T, SQLError>,
) -> Result<T, SQLError> {
    let label = projection_label(projection, control)?;
    let result = observe(label.as_str())?;
    control.check()?;
    Ok(result)
}

fn projection_label<'a>(
    projection: &'a ProjectionPlan,
    control: &ProductionControl<'_>,
) -> Result<Label<'a>, SQLError> {
    control.check()?;
    if let Some(alias) = &projection.alias {
        return Ok(Label::Borrowed(alias));
    }
    let mut expression = &projection.expr;
    while let ScalarExpr::Collated { expr, .. } = expression {
        control.check()?;
        expression = expr;
    }
    Ok(match expression {
        ScalarExpr::Column(column) | ScalarExpr::QualifiedColumn { column, .. } => {
            Label::Borrowed(column)
        }
        ScalarExpr::Star | ScalarExpr::QualifiedStar(_) => Label::Borrowed("*"),
        ScalarExpr::Func {
            name,
            binding,
            args,
            ..
        } => match field_selection_name(binding.as_ref(), args.get(1)) {
            Some(name) => Label::Borrowed(name),
            None => function_label(name, binding.as_ref(), control)?,
        },
        ScalarExpr::WindowCall { name, .. } => function_label(name, None, control)?,
        _ => Label::Borrowed("?column?"),
    })
}

/// `FigureColname` names a field selection by its field.
pub(crate) fn field_selection_label<E: FieldNameLiteral>(
    binding: Option<&FunctionBinding>,
    field: Option<&E>,
) -> Option<String> {
    field_selection_name(binding, field).map(str::to_owned)
}

fn field_selection_name<'a, E: FieldNameLiteral>(
    binding: Option<&FunctionBinding>,
    field: Option<&'a E>,
) -> Option<&'a str> {
    (binding.and_then(|binding| binding.dispatch) == Some(FunctionDispatch::FieldSelect))
        .then(|| field.and_then(FieldNameLiteral::field_name))
        .flatten()
}

/// The field name literal of a field selection in parsed or planned expressions.
pub(crate) trait FieldNameLiteral {
    fn field_name(&self) -> Option<&str>;
}

impl FieldNameLiteral for ScalarExpr {
    fn field_name(&self) -> Option<&str> {
        match self {
            ScalarExpr::Literal(uqa_core::Value::Str(field)) => Some(field),
            _ => None,
        }
    }
}

impl FieldNameLiteral for crate::ast::Expr {
    fn field_name(&self) -> Option<&str> {
        match self {
            crate::ast::Expr::Literal(uqa_core::Value::Str(field)) => Some(field),
            _ => None,
        }
    }
}

pub fn function_projection_label(name: &str, binding: Option<&FunctionBinding>) -> String {
    function_label(name, binding, &ProductionControl::uncontrolled())
        .expect("ordinary label decoding cannot be cancelled or limited")
        .into_uncontrolled()
}

pub(crate) fn rule_function_label<E: FieldNameLiteral>(
    name: &str,
    binding: Option<&FunctionBinding>,
    field: Option<&E>,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, SQLError> {
    if let Some(name) = field_selection_name(binding, field) {
        return Ok(control.copy_text(name)?);
    }
    let label = function_label(name, binding, control)?;
    Ok(control.copy_text(label.as_str())?)
}

fn function_label<'a>(
    name: &'a str,
    binding: Option<&FunctionBinding>,
    control: &ProductionControl<'_>,
) -> Result<Label<'a>, SQLError> {
    control.check()?;
    // Operators and other syntax that the parser lowers to calls have no name of their own.
    if matches!(
        binding.and_then(|binding| binding.dispatch),
        Some(
            FunctionDispatch::NumericOperator(_)
                | FunctionDispatch::Pattern { .. }
                | FunctionDispatch::Concat { .. }
                | FunctionDispatch::ArrayConcat
                | FunctionDispatch::ArrayAppend
                | FunctionDispatch::ArrayPrepend
                | FunctionDispatch::IsDistinct
                | FunctionDispatch::DistinctNot
                | FunctionDispatch::RowDistinct
                | FunctionDispatch::ValueIsDistinct
                | FunctionDispatch::ValueNullTest { .. }
                | FunctionDispatch::BooleanTest(_)
                | FunctionDispatch::AnyOperator
                | FunctionDispatch::AllOperator
                | FunctionDispatch::BetweenSymmetric
                | FunctionDispatch::JsonExtract { .. }
        )
    ) {
        return Ok(Label::Borrowed("?column?"));
    }
    let label = crate::compiler::parse_regobject_name_with_control(name, control)?
        .map_or(Label::Borrowed(name), Label::Decoded);
    if operator_call(label.as_str()) {
        return Ok(Label::Borrowed("?column?"));
    }
    Ok(label)
}

/// The calls the parser lowers operator syntax to under names no `PostgreSQL` function has: `||`, `@@`, `@?`, `@>`, `<@`, `&&`, `?`, `?|`, `?&`, `#-`, `~`, `~*`, `LIKE`, `ILIKE` and `SIMILAR TO`.
fn operator_call(name: &str) -> bool {
    matches!(
        name,
        "fts_match"
            | "jsonpath_exists"
            | "contains_op"
            | "contained_by_op"
            | "array_overlap"
            | "json_has_key"
            | "json_has_any_key"
            | "json_has_all_keys"
            | "json_delete_path"
    )
}

/// Observe ordered positional output labels without copying the input descriptor. Explicit source aliases override individual labels. A set operation inherits its left input's names; VALUES generates names for its first row. Return false only when the root has no output descriptor. Temporary decoded/generated labels retain their own leases through the callback and are released before the next label.
pub fn visit_query_output_columns(
    mut query: &QueryPlan,
    aliases: &[String],
    control: &ProductionControl<'_>,
    visitor: &mut impl FnMut(&str) -> Result<(), SQLError>,
) -> Result<bool, SQLError> {
    control.check()?;
    while let RelationalPlan::SetOp { left, .. } = &query.root {
        control.check()?;
        query = left;
    }
    match &query.root {
        RelationalPlan::QueryBlock(block) => {
            for (position, projection) in block.projections.iter().enumerate() {
                control.check()?;
                if let Some(alias) = aliases.get(position) {
                    visitor(alias)?;
                } else {
                    with_projection_label(projection, control, &mut *visitor)?;
                }
            }
        }
        RelationalPlan::Values { rows, .. } => {
            let Some(row) = rows.first() else {
                return Ok(false);
            };
            for position in 0..row.len() {
                control.check()?;
                if let Some(alias) = aliases.get(position) {
                    visitor(alias)?;
                } else {
                    let label = control.format(format_args!("column{}", position + 1))?;
                    visitor(&label)?;
                }
            }
        }
        RelationalPlan::SetOp { .. } => unreachable!("leftmost input"),
    }
    control.check()?;
    Ok(true)
}

#[cfg(test)]
mod tests;
