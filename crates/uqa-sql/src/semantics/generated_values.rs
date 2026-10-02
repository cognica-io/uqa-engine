//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` rules for the values a statement writes to the columns whose values a table generates: identity columns and generated columns.

use crate::{
    assignment::columns::AssignmentColumnCatalog,
    ast::{AutoIncrementKind, OverridingKind},
    SQLError,
};

/// How a table generates a column's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Generation {
    IdentityAlways,
    IdentityByDefault,
    Expression,
}

/// The identity and generated columns of one table, in column order, read once for a statement that writes it. `PostgreSQL` applies these rules when it rewrites the statement, column by column in table order, before it reads or writes a row, so a statement whose source yields no row is rejected as well.
pub struct GeneratedValueColumns {
    columns: Vec<(String, Generation)>,
}

impl GeneratedValueColumns {
    /// The identity and generated columns of `table`; none for a relation the catalog does not describe as a table.
    pub fn of(catalog: &dyn AssignmentColumnCatalog, table: &str) -> Result<Self, SQLError> {
        let columns = catalog
            .try_describe_table(table)
            .map_err(|error| SQLError::Internal(format!("read generated columns: {error}")))?
            .unwrap_or_default()
            .into_iter()
            .filter_map(|column| {
                let generation = match column.auto_increment.as_ref() {
                    Some(provenance) if provenance.kind == AutoIncrementKind::IdentityAlways => {
                        Generation::IdentityAlways
                    }
                    Some(provenance) if provenance.is_identity() => Generation::IdentityByDefault,
                    _ if column.generated.is_some() => Generation::Expression,
                    _ => return None,
                };
                Some((column.name, generation))
            })
            .collect();
        Ok(Self { columns })
    }

    /// Whether `column` is an identity column, whose value `OVERRIDING USER VALUE` leaves to its sequence.
    #[must_use]
    pub fn contains(&self, column: &str) -> bool {
        self.columns.iter().any(|(name, generation)| {
            name == column
                && matches!(
                    generation,
                    Generation::IdentityAlways | Generation::IdentityByDefault
                )
        })
    }

    /// Reject a value an `INSERT` supplies for a generated column, and for a `GENERATED ALWAYS` identity column without an `OVERRIDING` clause, which a generated column ignores. `targets` names each target column with whether some row supplies a value other than `DEFAULT` for it.
    pub fn validate_insert<'a>(
        &self,
        targets: impl IntoIterator<Item = (&'a str, bool)>,
        overriding: Option<OverridingKind>,
    ) -> Result<(), SQLError> {
        let supplied = targets
            .into_iter()
            .filter_map(|(column, supplied)| supplied.then_some(column))
            .collect::<std::collections::BTreeSet<_>>();
        for (column, generation) in &self.columns {
            if !supplied.contains(column.as_str()) {
                continue;
            }
            match generation {
                Generation::Expression => return Err(generated_column_insert_error(column)),
                Generation::IdentityAlways if overriding.is_none() => {
                    return Err(generated_always_insert_error(column));
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
        let assigned = assignments
            .into_iter()
            .filter_map(|(column, default)| (!default).then_some(column))
            .collect::<std::collections::BTreeSet<_>>();
        for (column, generation) in &self.columns {
            if !assigned.contains(column.as_str()) {
                continue;
            }
            match generation {
                Generation::Expression => return Err(generated_column_update_error(column)),
                Generation::IdentityAlways => return Err(generated_always_update_error(column)),
                Generation::IdentityByDefault => {}
            }
        }
        Ok(())
    }
}

/// The `428C9` error of a value an `INSERT` supplies for a `GENERATED ALWAYS` identity column.
#[must_use]
pub fn generated_always_insert_error(column: &str) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: "428C9".into(),
        message: format!("cannot insert a non-DEFAULT value into column \"{column}\""),
        detail: Some(format!(
            "Column \"{column}\" is an identity column defined as GENERATED ALWAYS."
        )),
        hint: Some("Use OVERRIDING SYSTEM VALUE to override.".into()),
    }
}

/// The `428C9` error of an assignment other than `DEFAULT` to a `GENERATED ALWAYS` identity column.
#[must_use]
pub fn generated_always_update_error(column: &str) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: "428C9".into(),
        message: format!("column \"{column}\" can only be updated to DEFAULT"),
        detail: Some(format!(
            "Column \"{column}\" is an identity column defined as GENERATED ALWAYS."
        )),
        hint: None,
    }
}

/// The `428C9` error of a value an `INSERT` supplies for a generated column.
#[must_use]
pub fn generated_column_insert_error(column: &str) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: "428C9".into(),
        message: format!("cannot insert a non-DEFAULT value into column \"{column}\""),
        detail: Some(format!("Column \"{column}\" is a generated column.")),
        hint: None,
    }
}

/// The `428C9` error of an assignment other than `DEFAULT` to a generated column.
#[must_use]
pub fn generated_column_update_error(column: &str) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: "428C9".into(),
        message: format!("column \"{column}\" can only be updated to DEFAULT"),
        detail: Some(format!("Column \"{column}\" is a generated column.")),
        hint: None,
    }
}
