//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected mutation names borrow declarations and retain only admitted lookup and target-reference workspace.

use super::AssignmentColumnCatalog;
use crate::{
    ast::{AssignmentTarget, ColumnDef},
    SQLError,
};
use uqa_core::{
    memory::{ProductionControl, ProductionMap, ProductionVec},
    RelationIdentity,
};

// Native and default metadata adapters call the reader exactly once and do no subsequent catalog work. Keep callback diagnostics separate from storage diagnostics so only the latter receive the caller's existing action context.
pub(crate) fn with_metadata<R>(
    catalog: &dyn AssignmentColumnCatalog,
    table: &str,
    control: &ProductionControl<'_>,
    source_error: impl FnOnce(SQLError) -> SQLError,
    read: impl FnOnce(&[ColumnDef]) -> Result<R, SQLError>,
) -> Result<Option<R>, SQLError> {
    let mut read = Some(read);
    let mut output = None;
    let found = catalog
        .with_column_metadata_with_control(table, control, &mut |columns| {
            output = Some(read.take().expect("one declaration generation")(columns));
            Ok(())
        })
        .map_err(source_error)?;
    if !found {
        return Ok(None);
    }
    output.expect("selected declaration generation").map(Some)
}

pub(crate) fn storage_context(
    error: SQLError,
    action: &str,
    control: &ProductionControl<'_>,
) -> SQLError {
    crate::catalog::errors::dml_storage_error_with_control(action, error, control)
}

pub fn validate_mutation_columns_with_control<'a>(
    catalog: &dyn AssignmentColumnCatalog,
    table: &str,
    columns: impl IntoIterator<Item = &'a str>,
    action: &str,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    validate_names(
        catalog,
        table,
        || columns.into_iter().map(Ok),
        action,
        control,
    )
}

pub fn validate_mutation_targets_with_control<'a, E: 'a>(
    catalog: &dyn AssignmentColumnCatalog,
    table: &str,
    targets: impl IntoIterator<Item = &'a AssignmentTarget<E>>,
    action: &str,
    insert: bool,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    let mut captured = ProductionVec::new(*control);
    for target in targets {
        captured.push_copy(target)?;
    }
    let captured = captured.finish()?;
    crate::assignment::targets::validate_repeated_targets_with_control(
        captured.iter().copied(),
        insert,
        control,
    )?;
    let mut distinct = ProductionMap::new(*control);
    let names = captured.iter().filter_map(|target| {
        let name = target.column.as_str();
        match distinct.insert_copy(name, ()) {
            Ok(None) => Some(Ok(name)),
            Ok(Some(())) => None,
            Err(error) => Some(Err(error.into())),
        }
    });
    validate_names(catalog, table, || names, action, control)
}

fn validate_names<'a, I: IntoIterator<Item = Result<&'a str, SQLError>>>(
    catalog: &dyn AssignmentColumnCatalog,
    table: &str,
    columns: impl FnOnce() -> I,
    action: &str,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    with_metadata(
        catalog,
        table,
        control,
        |error| storage_context(error, action, control),
        |definitions| {
            if definitions.is_empty()
                && !catalog
                    .columns_declared_with_control(table, control)
                    .map_err(|error| storage_context(error, action, control))?
            {
                return Ok(());
            }
            let mut known = ProductionMap::new(*control);
            for definition in definitions {
                known.insert_copy(definition.name.as_str(), ())?;
            }
            let mut seen = ProductionMap::new(*control);
            for column in columns() {
                let column = column?;
                if seen.insert_copy(column, ())?.is_some() {
                    return Err(SQLError::TypeMismatch(
                        control
                            .format(format_args!(
                                "{action}: column `{column}` is specified more than once"
                            ))?
                            .into_parts()
                            .0,
                    ));
                }
                if known.get(column).is_none() {
                    let relation = match RelationIdentity::try_borrowed_reference(table) {
                        Ok(relation) => relation,
                        Err(error) => {
                            return Err(SQLError::Internal(
                                control.format(format_args!("{error}"))?.into_parts().0,
                            ))
                        }
                    };
                    let state = control.copy_text("42703")?;
                    let message = control.format(format_args!(
                        "column \"{column}\" of relation \"{}\" does not exist",
                        relation.name
                    ))?;
                    return Err(SQLError::Routine {
                        sqlstate: state.into_parts().0,
                        message: message.into_parts().0,
                    });
                }
            }
            Ok(())
        },
    )?
    .ok_or_else(|| match control.copy_text(table) {
        Ok(name) => SQLError::UnknownTable(name.into_parts().0),
        Err(error) => error.into(),
    })
}

/// Own only declaration names; default and generated expressions remain with their catalog generation.
pub fn declared_column_names_with_control(
    catalog: &dyn AssignmentColumnCatalog,
    table: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<uqa_core::memory::Produced<Vec<String>>>, SQLError> {
    control.check()?;
    with_metadata(
        catalog,
        table,
        control,
        |error| error,
        |definitions| {
            let mut names = ProductionVec::new(*control);
            for definition in definitions {
                names.push_produced(control.copy_text(&definition.name)?)?;
            }
            Ok(names.finish()?)
        },
    )
}

#[cfg(test)]
mod tests;
