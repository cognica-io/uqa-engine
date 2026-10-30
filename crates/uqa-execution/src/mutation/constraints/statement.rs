//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The statement that writes a row, as constraint violations describe it.

/// The statement writing a row, as `PostgreSQL`'s executor knows it when it describes a row that fails a constraint (`ExecBuildSlotValueDescription`): the relation the statement names, whose columns describe a row it routes to a partition or updates in an inheritance child, and the columns it supplies, which a role that may not read the relation still sees.
#[derive(Debug, Clone, Copy)]
pub struct ConstraintStatement<'a> {
    /// The relation the statement names, by its catalog name (see [`statement_relation`]), or the table of the foreign key whose referential action writes the row.
    pub relation: &'a str,
    /// The columns the statement inserts or updates, by name.
    pub columns: &'a [String],
    /// Whether a referential action writes the row, which `PostgreSQL` performs as the owner of `relation`.
    pub referential_action: bool,
}

impl<'a> ConstraintStatement<'a> {
    /// A statement that names `relation` and supplies `columns`.
    pub const fn new(relation: &'a str, columns: &'a [String]) -> Self {
        Self {
            relation,
            columns,
            referential_action: false,
        }
    }

    /// The referential action of a foreign key of `relation` that sets `columns`.
    pub const fn referential_action(relation: &'a str, columns: &'a [String]) -> Self {
        Self {
            relation,
            columns,
            referential_action: true,
        }
    }
}

/// The catalog name of the relation a statement names, which its rows' tables are compared with; a name that resolves to no table is kept as written.
pub fn statement_relation(
    context: super::ConstraintContext<'_>,
    relation: &str,
) -> Result<String, uqa_sql::SQLError> {
    statement_relation_with_control(
        context,
        relation,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .map(|name| {
        name.into_uncontrolled()
            .expect("ordinary statement relation")
    })
}

/// Resolve before falling back to the supplied spelling, retaining the selected diagnostic name with its producer's allowance.
pub fn statement_relation_with_control(
    context: super::ConstraintContext<'_>,
    relation: &str,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<uqa_core::memory::Produced<String>, uqa_sql::SQLError> {
    let resolved = context
        .partitions
        .catalog
        .try_resolve_table_name_with_control(relation, control)
        .map_err(|error| match error {
            uqa_sql::SQLError::Internal(message) => {
                match control.format(format_args!("resolve statement relation: {message}")) {
                    Ok(message) => uqa_sql::SQLError::Internal(message.into_parts().0),
                    Err(error) => error.into(),
                }
            }
            error => error,
        })?;
    match resolved {
        Some(name) => Ok(name),
        None => Ok(control.copy_text(relation)?),
    }
}
