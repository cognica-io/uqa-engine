//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` rules for the values a statement writes to identity columns.

use crate::{
    assignment::columns::AssignmentColumnCatalog,
    ast::{AutoIncrementKind, OverridingKind},
    SQLError,
};

/// The identity columns of one table, read once for a statement that writes it. `PostgreSQL` applies these rules when it rewrites the statement, before it reads or writes a row, so a statement whose source yields no row is rejected as well.
pub struct IdentityColumns {
    /// Each identity column, and whether it is `GENERATED ALWAYS`.
    columns: Vec<(String, bool)>,
}

impl IdentityColumns {
    /// The identity columns of `table`; none for a relation the catalog does not describe as a table.
    pub fn of(catalog: &dyn AssignmentColumnCatalog, table: &str) -> Result<Self, SQLError> {
        let columns = catalog
            .try_describe_table(table)
            .map_err(|error| SQLError::Internal(format!("read identity columns: {error}")))?
            .unwrap_or_default()
            .into_iter()
            .filter_map(|column| {
                let provenance = column.auto_increment.as_ref()?;
                provenance.is_identity().then(|| {
                    let always = provenance.kind == AutoIncrementKind::IdentityAlways;
                    (column.name, always)
                })
            })
            .collect();
        Ok(Self { columns })
    }

    /// Whether `column` is an identity column, whose value `OVERRIDING USER VALUE` leaves to its sequence.
    #[must_use]
    pub fn contains(&self, column: &str) -> bool {
        self.columns.iter().any(|(identity, _)| identity == column)
    }

    fn always(&self, column: &str) -> bool {
        self.columns
            .iter()
            .any(|(identity, always)| *always && identity == column)
    }

    /// Reject a value an `INSERT` supplies for a `GENERATED ALWAYS` column without an `OVERRIDING` clause. `targets` names each target column with whether some row supplies a value other than `DEFAULT` for it.
    pub fn validate_insert<'a>(
        &self,
        targets: impl IntoIterator<Item = (&'a str, bool)>,
        overriding: Option<OverridingKind>,
    ) -> Result<(), SQLError> {
        if overriding.is_some() {
            return Ok(());
        }
        for (column, supplied) in targets {
            if supplied && self.always(column) {
                return Err(generated_always_insert_error(column));
            }
        }
        Ok(())
    }

    /// Reject an assignment other than `DEFAULT` to a `GENERATED ALWAYS` column by an `UPDATE`, an `ON CONFLICT DO UPDATE` or a `MERGE` update. `assignments` names each assigned column with whether its value is `DEFAULT`.
    pub fn validate_update<'a>(
        &self,
        assignments: impl IntoIterator<Item = (&'a str, bool)>,
    ) -> Result<(), SQLError> {
        for (column, default) in assignments {
            if !default && self.always(column) {
                return Err(generated_always_update_error(column));
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
        detail: Some(generated_always_detail(column)),
        hint: Some("Use OVERRIDING SYSTEM VALUE to override.".into()),
    }
}

/// The `428C9` error of an assignment other than `DEFAULT` to a `GENERATED ALWAYS` identity column.
#[must_use]
pub fn generated_always_update_error(column: &str) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: "428C9".into(),
        message: format!("column \"{column}\" can only be updated to DEFAULT"),
        detail: Some(generated_always_detail(column)),
        hint: None,
    }
}

fn generated_always_detail(column: &str) -> String {
    format!("Column \"{column}\" is an identity column defined as GENERATED ALWAYS.")
}
