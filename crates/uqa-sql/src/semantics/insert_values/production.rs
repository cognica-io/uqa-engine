//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Single-row VALUES expansion admits its discovery buffers and transfers the selected command fields.

use super::ViewRewriteContext;
use crate::{
    catalog::view::StoredViewKind,
    plan::{copy::SyntaxProduction, InsertPlan, MutableCommandPlan, RuleValuesInput},
    production::Buffer,
    SQLError, ScalarExpr,
};
use uqa_core::memory::{OwnedSet, Produced, ProductionControl, ProductionVec};

pub fn single_row_values_query_with_control(
    routines: &dyn crate::routines::RoutineResolution,
    insert: &InsertPlan,
    observed: impl FnOnce() -> Result<Option<Produced<Vec<String>>>, SQLError>,
    implicit_columns: impl FnOnce() -> Result<Option<Produced<Vec<String>>>, SQLError>,
    control: &ProductionControl<'_>,
) -> Result<Option<MutableCommandPlan>, SQLError> {
    control.check()?;
    let [row] = insert.rows.as_slice() else {
        return Ok(None);
    };
    let mut set_valued = ProductionVec::new(*control);
    for value in row {
        set_valued.push_copy(
            crate::semantics::sets::static_setness::expression_may_return_set_statically_with_control(
                routines, value, control,
            )?,
        )?;
    }
    if !set_valued.contains(&true) {
        return Ok(None);
    }
    let Some(observed) = observed()? else {
        return Err(crate::semantics::sets::placement::set_valued_call_error());
    };
    let defaults = row.iter().any(|value| matches!(value, ScalarExpr::Default));
    let names = if insert.columns.is_empty() && (defaults || !observed.is_empty()) {
        let Some(columns) = implicit_columns()? else {
            return Ok(None);
        };
        Some(columns)
    } else {
        None
    };
    let explicit = !insert.columns.is_empty() || defaults;
    let width = names
        .as_ref()
        .map_or(insert.columns.len(), |names| names.len().min(row.len()));
    if (explicit || width != 0) && width != row.len() {
        return Ok(None);
    }
    for (position, returns_set) in set_valued.iter().enumerate() {
        control.check()?;
        let name = insert
            .columns
            .get(position)
            .map(|target| target.column.as_str())
            .or_else(|| names.as_ref()?.get(position).map(String::as_str));
        if *returns_set && name.is_some_and(|name| observed.iter().any(|column| column == name)) {
            return Err(crate::semantics::sets::placement::set_valued_call_error());
        }
    }
    drop(observed);
    drop(set_valued);
    let mut output = insert.copy_for_rewrite(control)?;
    if defaults && insert.columns.is_empty() {
        output = output.set_implicit_insert_columns(
            names
                .as_ref()
                .expect("implicit DEFAULT targets")
                .iter()
                .take(row.len())
                .map(String::as_str),
            control,
        )?;
    }
    drop(names);
    if explicit {
        output = output.retain_insert_columns(control, |position, _| {
            Ok(!matches!(row[position], ScalarExpr::Default))
        })?;
    }
    let (output, mut source) = output.take_insert_rows_query(control)?;
    if defaults {
        source = source.map_value_rows(control, |row| {
            let mut selected = Buffer::new(control);
            for value in row {
                if !matches!(&*value, ScalarExpr::Default) {
                    selected.push(value, control)?;
                }
            }
            Ok(selected.into_iter())
        })?;
    }
    let source = source.project_values(control)?;
    let width = row
        .iter()
        .filter(|value| !matches!(value, ScalarExpr::Default))
        .count();
    let metadata = SyntaxProduction::new(*control);
    let input = RuleValuesInput {
        width,
        inline: true,
        defaults: Vec::new(),
        unresolved: OwnedSet::new(),
        written: metadata.set_copy(0..width)?,
    };
    let output = output.set_rule_values_input(metadata.finish(input)?, control)?;
    Ok(Some(output.map_insert_source(control, |_, _| Ok(source))?))
}

pub fn implicit_target_columns_with_control(
    context: ViewRewriteContext<'_>,
    table: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Vec<String>>>, SQLError> {
    match context
        .catalog
        .target_view_kind_with_control(table, control)?
    {
        Some(StoredViewKind::View) => {
            let target = crate::semantics::view_mutation::resolve_view_target_with_control(
                context, table, control,
            )?;
            let mut columns = ProductionVec::new(*control);
            for column in &target.columns {
                columns.push_produced(control.copy_text(column)?)?;
            }
            Ok(Some(columns.finish()?))
        }
        Some(StoredViewKind::Materialized) => Ok(None),
        None => context
            .catalog
            .try_table_column_names_with_control(table, control)
            .map(Some),
    }
}
