//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Locate durable constraints and analyze changes to their type, identity, and enforcement metadata.
use crate::schema::foreign_keys::column_foreign_key;
use crate::{
    ast::{ColumnType, ForeignKey, TableCheck},
    SQLError,
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConstraintLocation {
    NotNull(usize),
    ColumnCheck(usize),
    ColumnForeignKey(usize),
    TableCheck(usize),
    TableForeignKey(usize),
    Key(usize),
}

pub fn constraint_error(sqlstate: &str, message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message: message.into(),
    }
}

pub fn find_constraint(
    columns: &[crate::ast::ColumnDef],
    constraints: &crate::ast::TableConstraintSet,
    name: &str,
) -> Option<ConstraintLocation> {
    columns
        .iter()
        .position(|column| column.not_null && column.not_null_name.as_deref() == Some(name))
        .map(ConstraintLocation::NotNull)
        .or_else(|| {
            columns
                .iter()
                .position(|column| {
                    column.check.is_some() && column.check_name.as_deref() == Some(name)
                })
                .map(ConstraintLocation::ColumnCheck)
        })
        .or_else(|| {
            columns
                .iter()
                .position(|column| {
                    column
                        .references
                        .as_ref()
                        .and_then(|reference| reference.name.as_deref())
                        == Some(name)
                })
                .map(ConstraintLocation::ColumnForeignKey)
        })
        .or_else(|| {
            constraints
                .checks
                .iter()
                .position(|constraint| constraint.name.as_deref() == Some(name))
                .map(ConstraintLocation::TableCheck)
        })
        .or_else(|| {
            constraints
                .foreign_keys
                .iter()
                .position(|constraint| constraint.name.as_deref() == Some(name))
                .map(ConstraintLocation::TableForeignKey)
        })
        .or_else(|| {
            constraints
                .key_constraints
                .iter()
                .position(|constraint| constraint.name.as_deref() == Some(name))
                .map(ConstraintLocation::Key)
        })
}

pub fn ensure_constraint_name_available(
    columns: &[crate::ast::ColumnDef],
    constraints: &crate::ast::TableConstraintSet,
    name: Option<&str>,
    table: &str,
) -> Result<(), SQLError> {
    if let Some(name) = name.filter(|name| find_constraint(columns, constraints, name).is_some()) {
        return Err(constraint_error(
            "42710",
            format!("constraint \"{name}\" for relation \"{table}\" already exists"),
        ));
    }
    Ok(())
}

pub fn ensure_not_null_inheritable(
    table: &str,
    column: &crate::ast::ColumnDef,
    sqlstate: &str,
) -> Result<(), SQLError> {
    if column.not_null_no_inherit {
        let relation = uqa_core::RelationIdentity::from_legacy_name(table)
            .map_err(|error| SQLError::Internal(format!("resolve NOT NULL relation: {error}")))?;
        let name = column.not_null_name.as_deref().unwrap_or("<unnamed>");
        return Err(constraint_error(
            sqlstate,
            format!(
            "cannot change NO INHERIT status of NOT NULL constraint \"{name}\" on relation \"{}\"",
            relation.name,
        ),
        ));
    }
    Ok(())
}

pub fn take_column_check(column: &mut crate::ast::ColumnDef) -> Option<TableCheck> {
    let check = TableCheck {
        expr: column.check.take()?,
        name: column.check_name.take(),
        object_id: column.check_object_id.take(),
        is_local: column.check_is_local,
        enforced: column.check_enforced,
        validated: column.check_validated,
        no_inherit: column.check_no_inherit,
        partition_constraint: None,
    };
    column.check_is_local = true;
    column.check_enforced = true;
    column.check_validated = true;
    column.check_no_inherit = false;
    Some(check)
}

pub fn foreign_key_object_id(
    columns: &[crate::ast::ColumnDef],
    constraints: &crate::ast::TableConstraintSet,
    location: ConstraintLocation,
) -> Option<[u8; 16]> {
    match location {
        ConstraintLocation::ColumnForeignKey(index) => columns[index]
            .references
            .as_ref()
            .and_then(|reference| reference.object_id),
        ConstraintLocation::TableForeignKey(index) => constraints.foreign_keys[index].object_id,
        _ => None,
    }
}

pub trait ConstraintTypeReferrers {
    fn try_referrers_to(
        &self,
        table: &str,
    ) -> Result<Vec<(String, ForeignKey)>, crate::assignment::columns::ColumnCatalogError>;
}
pub struct ConstraintTypeContext<'a> {
    pub foreign_keys: crate::schema::foreign_keys::ForeignKeyDefinitionContext<'a>,
    pub referrers: &'a dyn ConstraintTypeReferrers,
}
fn ddl_storage_error(
    action: &str,
    error: crate::assignment::columns::ColumnCatalogError,
) -> SQLError {
    crate::catalog::errors::storage_error(action, error.as_ref())
}
pub fn validate_altered_constraint_column_types(
    context: &ConstraintTypeContext<'_>,
    table: &str,
    candidate_columns: &[crate::ast::ColumnDef],
    key_constraints: &[crate::ast::TableKeyConstraint],
    foreign_keys: &[ForeignKey],
) -> Result<(), SQLError> {
    for constraint in key_constraints
        .iter()
        .filter(|constraint| constraint.without_overlaps)
    {
        let Some(period_column) = constraint.columns.last() else {
            return Err(SQLError::Internal(
                "WITHOUT OVERLAPS constraint has no period column".into(),
            ));
        };
        let period_type = candidate_columns
            .iter()
            .find(|column| column.name == *period_column)
            .map(|column| &column.ty)
            .ok_or_else(|| SQLError::UnknownColumn(format!("{table}.{period_column}")))?;
        if !matches!(
            period_type,
            ColumnType::Range(_) | ColumnType::Multirange(_)
        ) {
            return Err(SQLError::Routine {
                sqlstate: "42804".into(),
                message: format!(
                    "column \"{period_column}\" in WITHOUT OVERLAPS is not a range or multirange type"
                ),
            });
        }
    }

    for foreign_key in foreign_keys.iter().filter(|foreign_key| foreign_key.period) {
        let (parent_name, parent_columns, parent_keys) =
            crate::schema::foreign_keys::resolve_foreign_key_parent(
                &context.foreign_keys,
                &foreign_key.ref_table,
            )?;
        let parent_columns = if parent_name == table {
            candidate_columns
        } else {
            parent_columns.as_slice()
        };
        crate::schema::constraints::validate_foreign_key_definition(
            table,
            candidate_columns,
            &parent_name,
            parent_columns,
            &parent_keys,
            foreign_key,
        )?;
    }

    for (child_table, foreign_key) in context
        .referrers
        .try_referrers_to(table)
        .map_err(|error| ddl_storage_error("ALTER COLUMN TYPE", error))?
        .into_iter()
        .filter(|(_, foreign_key)| foreign_key.period)
    {
        let child_columns = if child_table == table {
            candidate_columns.to_vec()
        } else {
            context
                .foreign_keys
                .columns
                .try_describe_table(&child_table)
                .map_err(|error| ddl_storage_error("ALTER COLUMN TYPE", error))?
                .ok_or_else(|| SQLError::UnknownTable(child_table.clone()))?
        };
        crate::schema::constraints::validate_foreign_key_definition(
            &child_table,
            &child_columns,
            table,
            candidate_columns,
            key_constraints,
            &foreign_key,
        )?;
    }
    Ok(())
}

pub struct ConstraintAlterOptions {
    pub enforceability: Option<bool>,
    pub deferrability: Option<(bool, bool)>,
    pub no_inherit: Option<bool>,
}
pub struct ConstraintAlterEffects {
    pub recreated_foreign_key: Option<ForeignKey>,
    pub validate_after_publish: bool,
}
#[expect(
    clippy::too_many_lines,
    reason = "preserves ordered constraint alteration rules"
)]
pub fn apply_constraint_alteration(
    table: &str,
    name: &str,
    columns: &mut [crate::ast::ColumnDef],
    constraints: &mut crate::ast::TableConstraintSet,
    options: ConstraintAlterOptions,
) -> Result<ConstraintAlterEffects, SQLError> {
    let ConstraintAlterOptions {
        enforceability,
        deferrability,
        no_inherit,
    } = options;
    let location = find_constraint(columns, constraints, name).ok_or_else(|| {
        constraint_error(
            "42704",
            format!("constraint \"{name}\" of relation \"{table}\" does not exist"),
        )
    })?;
    let is_foreign_key = matches!(
        location,
        ConstraintLocation::ColumnForeignKey(_) | ConstraintLocation::TableForeignKey(_)
    );
    let is_not_null = matches!(location, ConstraintLocation::NotNull(_));
    if enforceability.is_some() && !is_foreign_key {
        return Err(constraint_error(
            "42809",
            format!("cannot alter enforceability of constraint \"{name}\" of relation \"{table}\""),
        ));
    }
    if deferrability.is_some() && !is_foreign_key {
        return Err(constraint_error(
            "42809",
            format!(
                "constraint \"{name}\" of relation \"{table}\" is not a foreign key constraint"
            ),
        ));
    }
    if no_inherit.is_some() && !is_not_null {
        return Err(constraint_error(
            "42809",
            format!("constraint \"{name}\" of relation \"{table}\" is not a not-null constraint"),
        ));
    }
    let recreated_foreign_key = if enforceability == Some(true) {
        match location {
            ConstraintLocation::ColumnForeignKey(index) => columns[index]
                .references
                .as_ref()
                .filter(|foreign_key| !foreign_key.enforced)
                .map(|foreign_key| column_foreign_key(&columns[index], foreign_key)),
            ConstraintLocation::TableForeignKey(index) => constraints
                .foreign_keys
                .get(index)
                .filter(|foreign_key| !foreign_key.enforced)
                .cloned(),
            ConstraintLocation::NotNull(_)
            | ConstraintLocation::ColumnCheck(_)
            | ConstraintLocation::TableCheck(_)
            | ConstraintLocation::Key(_) => None,
        }
    } else {
        None
    };
    let mut validate_after_publish = false;
    match location {
        ConstraintLocation::NotNull(index) => {
            if let Some(no_inherit) = no_inherit {
                columns[index].not_null_no_inherit = no_inherit;
            }
        }
        ConstraintLocation::ColumnForeignKey(index) => {
            let foreign_key = columns[index]
                .references
                .as_mut()
                .ok_or_else(|| SQLError::Internal("column FOREIGN KEY disappeared".into()))?;
            if let Some(enforced) = enforceability {
                if !enforced {
                    foreign_key.enforced = false;
                    foreign_key.validated = false;
                } else if !foreign_key.enforced {
                    foreign_key.enforced = true;
                    foreign_key.validated = false;
                    validate_after_publish = true;
                }
            }
            if let Some((deferrable, initially_deferred)) = deferrability {
                foreign_key.deferrable = deferrable;
                foreign_key.initially_deferred = initially_deferred;
            }
        }
        ConstraintLocation::TableForeignKey(index) => {
            let foreign_key = &mut constraints.foreign_keys[index];
            if let Some(enforced) = enforceability {
                if !enforced {
                    foreign_key.enforced = false;
                    foreign_key.validated = false;
                } else if !foreign_key.enforced {
                    foreign_key.enforced = true;
                    foreign_key.validated = false;
                    validate_after_publish = true;
                }
            }
            if let Some((deferrable, initially_deferred)) = deferrability {
                foreign_key.deferrable = deferrable;
                foreign_key.initially_deferred = initially_deferred;
            }
        }
        ConstraintLocation::ColumnCheck(_)
        | ConstraintLocation::TableCheck(_)
        | ConstraintLocation::Key(_) => {}
    }
    Ok(ConstraintAlterEffects {
        recreated_foreign_key,
        validate_after_publish,
    })
}
