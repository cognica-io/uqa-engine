//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` rules for the values a statement writes to the columns whose values a table generates: identity columns and generated columns.

use crate::{
    assignment::columns::{production::with_metadata, AssignmentColumnCatalog},
    ast::{AutoIncrementKind, OverridingKind},
    SQLError,
};

use uqa_core::memory::{Produced, ProductionControl, ProductionMap, ProductionVec};

/// Retain user-supplied value provenance after a rule rewrites VALUES into a query source.
pub fn insert_has_written_value(insert: &crate::plan::InsertPlan, position: usize) -> bool {
    if let Some(values) = &insert.rule_values_input {
        return values.has_written_value(position);
    }
    insert.source.is_some()
        || insert.rows.iter().any(|row| {
            row.get(position)
                .is_some_and(|value| !matches!(value, crate::ScalarExpr::Default))
        })
}

/// Stored expressions affected by an UPDATE's target columns. Explicitly assigning an unchanged input still selects its dependents; assigning a generated column to DEFAULT does not select it by itself. Callers with BEFORE ROW UPDATE triggers must recompute every stored expression instead.
#[must_use]
pub fn stored_columns_for_update(
    columns: &[crate::ast::ColumnDef],
    updated_columns: &[String],
) -> Vec<String> {
    columns
        .iter()
        .filter(|column| {
            column.generated.as_ref().is_some_and(|generated| {
                generated.kind == crate::ast::GeneratedColumnKind::Stored
                    && updated_columns.iter().any(|updated| {
                        crate::schema::dependencies::schema_expr_references_column(
                            &generated.expression,
                            updated,
                        )
                    })
            })
        })
        .map(|column| column.name.clone())
        .collect()
}

/// How a table generates a column's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Generation {
    IdentityAlways,
    IdentityByDefault,
    Expression,
}

/// The identity and generated columns of one table, in column order, read once for a statement that writes it. `PostgreSQL` applies these rules when it rewrites the statement, column by column in table order, before it reads or writes a row, so a statement whose source yields no row is rejected as well.
pub struct GeneratedValueColumns {
    columns: Produced<Vec<(String, Generation)>>,
}

impl GeneratedValueColumns {
    /// The identity and generated columns of `table`; none for a relation the catalog does not describe as a table.
    pub fn of(catalog: &dyn AssignmentColumnCatalog, table: &str) -> Result<Self, SQLError> {
        Self::of_with_control(catalog, table, &ProductionControl::uncontrolled())
    }

    pub fn of_with_control(
        catalog: &dyn AssignmentColumnCatalog,
        table: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let mut columns = ProductionVec::new(*control);
        with_metadata(
            catalog,
            table,
            control,
            |error| match error {
                SQLError::Internal(message) => {
                    match control.format(format_args!("read generated columns: {message}")) {
                        Ok(message) => SQLError::Internal(message.into_parts().0),
                        Err(error) => error.into(),
                    }
                }
                error => error,
            },
            |declarations| {
                for column in declarations {
                    control.check()?;
                    let generation = match column.auto_increment.as_ref() {
                        Some(provenance)
                            if provenance.kind == AutoIncrementKind::IdentityAlways =>
                        {
                            Generation::IdentityAlways
                        }
                        Some(provenance) if provenance.is_identity() => {
                            Generation::IdentityByDefault
                        }
                        _ if column.generated.is_some() => Generation::Expression,
                        _ => continue,
                    };
                    let (name, memory) = control.copy_text(&column.name)?.into_parts();
                    columns.push_produced(control.finish((name, generation), memory)?)?;
                }
                Ok(())
            },
        )?;
        Ok(Self {
            columns: columns.finish()?,
        })
    }

    /// Whether `column` is an identity column, whose value `OVERRIDING USER VALUE` leaves to its sequence.
    #[must_use]
    pub fn contains(&self, column: &str) -> bool {
        self.contains_with_control(column, &ProductionControl::uncontrolled())
            .expect("ordinary generated-column lookup")
    }

    pub fn contains_with_control(
        &self,
        column: &str,
        control: &ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        for (name, generation) in self.columns.iter() {
            control.check()?;
            if name == column
                && matches!(
                    generation,
                    Generation::IdentityAlways | Generation::IdentityByDefault
                )
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Reject a value an `INSERT` supplies for a generated column, and for a `GENERATED ALWAYS` identity column without an `OVERRIDING` clause, which a generated column ignores. `targets` names each target column with whether some row supplies a value other than `DEFAULT` for it.
    pub fn validate_insert<'a>(
        &self,
        targets: impl IntoIterator<Item = (&'a str, bool)>,
        overriding: Option<OverridingKind>,
    ) -> Result<(), SQLError> {
        self.validate_insert_with_control(targets, overriding, &ProductionControl::uncontrolled())
    }

    pub fn validate_insert_with_control<'a>(
        &self,
        targets: impl IntoIterator<Item = (&'a str, bool)>,
        overriding: Option<OverridingKind>,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        let mut supplied = ProductionMap::new(*control);
        for (column, written) in targets {
            control.check()?;
            if written {
                supplied.insert_copy(column, ())?;
            }
        }
        for (column, generation) in self.columns.iter() {
            control.check()?;
            if supplied.get(column.as_str()).is_none() {
                continue;
            }
            match generation {
                Generation::Expression => return Err(value_error(column, true, false, control)?),
                Generation::IdentityAlways if overriding.is_none() => {
                    return Err(value_error(column, true, true, control)?)
                }
                Generation::IdentityAlways | Generation::IdentityByDefault => {}
            }
        }
        Ok(())
    }

    /// Reject an assignment other than `DEFAULT` to a generated column or a `GENERATED ALWAYS` identity column by an `UPDATE`, an `ON CONFLICT DO UPDATE` or a `MERGE` update. `assignments` names each assigned column with whether its value is `DEFAULT`.
    pub fn validate_update<'a>(
        &self,
        assignments: impl IntoIterator<Item = (&'a str, bool)>,
    ) -> Result<(), SQLError> {
        self.validate_update_with_control(assignments, &ProductionControl::uncontrolled())
    }

    pub fn validate_update_with_control<'a>(
        &self,
        assignments: impl IntoIterator<Item = (&'a str, bool)>,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        let mut assigned = ProductionMap::new(*control);
        for (column, default) in assignments {
            control.check()?;
            if !default {
                assigned.insert_copy(column, ())?;
            }
        }
        for (column, generation) in self.columns.iter() {
            control.check()?;
            if assigned.get(column.as_str()).is_none() {
                continue;
            }
            match generation {
                Generation::Expression => return Err(value_error(column, false, false, control)?),
                Generation::IdentityAlways => {
                    return Err(value_error(column, false, true, control)?)
                }
                Generation::IdentityByDefault => {}
            }
        }
        Ok(())
    }
}

fn value_error(
    column: &str,
    insert: bool,
    identity: bool,
    control: &ProductionControl<'_>,
) -> Result<SQLError, uqa_core::ValueRetentionError> {
    let sqlstate = control.copy_text("428C9")?;
    let message = if insert {
        control.format(format_args!(
            "cannot insert a non-DEFAULT value into column \"{column}\""
        ))?
    } else {
        control.format(format_args!(
            "column \"{column}\" can only be updated to DEFAULT"
        ))?
    };
    let detail = if identity {
        control.format(format_args!(
            "Column \"{column}\" is an identity column defined as GENERATED ALWAYS."
        ))?
    } else {
        control.format(format_args!("Column \"{column}\" is a generated column."))?
    };
    let hint = (insert && identity)
        .then(|| control.copy_text("Use OVERRIDING SYSTEM VALUE to override."))
        .transpose()?;
    Ok(SQLError::Diagnostic {
        sqlstate: sqlstate.into_parts().0,
        message: message.into_parts().0,
        detail: Some(detail.into_parts().0),
        hint: hint.map(|hint| hint.into_parts().0),
    })
}

/// The `428C9` error of a value an `INSERT` supplies for a `GENERATED ALWAYS` identity column.
#[must_use]
pub fn generated_always_insert_error(column: &str) -> SQLError {
    value_error(column, true, true, &ProductionControl::uncontrolled()).unwrap_or_else(Into::into)
}

/// The `428C9` error of an assignment other than `DEFAULT` to a `GENERATED ALWAYS` identity column.
#[must_use]
pub fn generated_always_update_error(column: &str) -> SQLError {
    value_error(column, false, true, &ProductionControl::uncontrolled()).unwrap_or_else(Into::into)
}

/// The `428C9` error of a value an `INSERT` supplies for a generated column.
#[must_use]
pub fn generated_column_insert_error(column: &str) -> SQLError {
    value_error(column, true, false, &ProductionControl::uncontrolled()).unwrap_or_else(Into::into)
}

/// The `428C9` error of an assignment other than `DEFAULT` to a generated column.
#[must_use]
pub fn generated_column_update_error(column: &str) -> SQLError {
    value_error(column, false, false, &ProductionControl::uncontrolled()).unwrap_or_else(Into::into)
}

#[cfg(test)]
mod tests {
    #[test]
    fn stored_update_selection_follows_expression_inputs() {
        let crate::Statement::CreateTable(table) = crate::compile("CREATE TABLE t(a int,b int,unrelated int,ga int GENERATED ALWAYS AS(coalesce(a,1)) STORED,gb int GENERATED ALWAYS AS(b+2) STORED,c int GENERATED ALWAYS AS(42) STORED,v int GENERATED ALWAYS AS(b+3) VIRTUAL)").unwrap().remove(0) else { panic!("table") };
        for (targets, expected) in [
            (vec!["a"], vec!["ga"]),
            (vec!["b"], vec!["gb"]),
            (vec!["a", "b"], vec!["ga", "gb"]),
            (vec!["unrelated"], vec![]),
            (vec!["ga", "c", "v"], vec![]),
        ] {
            let targets = targets.into_iter().map(str::to_owned).collect::<Vec<_>>();
            assert_eq!(
                super::stored_columns_for_update(&table.columns, &targets),
                expected
            );
        }
    }
}
