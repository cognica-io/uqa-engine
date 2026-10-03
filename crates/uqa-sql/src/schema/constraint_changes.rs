//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Locate durable constraints and analyze changes to their type, identity, and enforcement metadata.
pub mod foreign_key_target;
pub mod inheritance;
pub mod names;
pub mod not_null_removal;
pub mod renaming;
pub mod validation;

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
    /// A constraint the foreign key at a location derives on a referenced partition, by its position among that foreign key's derived constraints.
    ReferencedPartition(ForeignKeyLocation, usize),
}

/// Where a relation declares a foreign key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForeignKeyLocation {
    Column(usize),
    Table(usize),
}

impl ForeignKeyLocation {
    pub const fn constraint(self) -> ConstraintLocation {
        match self {
            Self::Column(index) => ConstraintLocation::ColumnForeignKey(index),
            Self::Table(index) => ConstraintLocation::TableForeignKey(index),
        }
    }

    /// The name and derived constraints of the foreign key at this location.
    pub fn derived<'a>(
        self,
        columns: &'a [crate::ast::ColumnDef],
        constraints: &'a crate::ast::TableConstraintSet,
    ) -> Option<(&'a str, &'a [crate::ast::ReferencedPartitionConstraint])> {
        match self {
            Self::Column(index) => columns
                .get(index)?
                .references
                .as_ref()
                .and_then(|reference| {
                    Some((
                        reference.name.as_deref()?,
                        reference.referenced_partitions.as_slice(),
                    ))
                }),
            Self::Table(index) => constraints.foreign_keys.get(index).and_then(|foreign_key| {
                Some((
                    foreign_key.name.as_deref()?,
                    foreign_key.referenced_partitions.as_slice(),
                ))
            }),
        }
    }

    /// The derived constraints of the foreign key at this location, for an edit.
    pub fn derived_mut<'a>(
        self,
        columns: &'a mut [crate::ast::ColumnDef],
        constraints: &'a mut crate::ast::TableConstraintSet,
    ) -> Option<&'a mut Vec<crate::ast::ReferencedPartitionConstraint>> {
        match self {
            Self::Column(index) => columns
                .get_mut(index)?
                .references
                .as_mut()
                .map(|reference| &mut reference.referenced_partitions),
            Self::Table(index) => constraints
                .foreign_keys
                .get_mut(index)
                .map(|foreign_key| &mut foreign_key.referenced_partitions),
        }
    }
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
    names::ConstraintNames::from_definition(columns, constraints)
        .entries()
        .find(|constraint| constraint.name == name)
        .map(|constraint| constraint.location)
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
        catalog_oid: column.check_catalog_oid.take(),
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
    /// The constraint without a parent that the altered constraint derives from, when it is a partition's copy of a foreign key.
    pub ancestor: Option<ConstraintAncestor>,
}

/// A constraint without a parent, which the constraints derived from it name in diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstraintAncestor {
    pub name: String,
    pub table: String,
}

/// `ALTER CONSTRAINT` of a constraint derived from another, which `PostgreSQL` refuses so that the derived constraints keep following the one they derive from.
pub fn derived_constraint_alteration(
    table: &str,
    name: &str,
    ancestor: &ConstraintAncestor,
) -> SQLError {
    let local = |table: &str| match uqa_core::RelationIdentity::from_legacy_name(table) {
        Ok(relation) => Ok(relation.name),
        Err(error) => Err(SQLError::Internal(error)),
    };
    let (relation, ancestor_relation) = match (local(table), local(&ancestor.table)) {
        (Ok(relation), Ok(ancestor_relation)) => (relation, ancestor_relation),
        (Err(error), _) | (_, Err(error)) => return error,
    };
    SQLError::Diagnostic {
        sqlstate: "55000".into(),
        message: format!("cannot alter constraint \"{name}\" on relation \"{relation}\""),
        detail: Some(format!(
            "Constraint \"{name}\" is derived from constraint \"{}\" of relation \"{ancestor_relation}\".",
            ancestor.name
        )),
        hint: Some("You may alter the constraint it derives from instead.".into()),
    }
}
pub struct ConstraintAlterEffects {
    pub recreated_foreign_key: Option<ForeignKey>,
    pub validate_after_publish: bool,
}
/// The kind of constraint each requested change applies to: enforceability and deferrability to foreign keys, inheritability to `NOT NULL` constraints.
fn ensure_alteration_applies(
    location: ConstraintLocation,
    relation: &str,
    name: &str,
    enforceability: bool,
    deferrability: bool,
    no_inherit: bool,
) -> Result<(), SQLError> {
    let is_foreign_key = matches!(
        location,
        ConstraintLocation::ColumnForeignKey(_)
            | ConstraintLocation::TableForeignKey(_)
            | ConstraintLocation::ReferencedPartition(..)
    );
    if enforceability && !is_foreign_key {
        return Err(constraint_error(
            "42809",
            format!(
                "cannot alter enforceability of constraint \"{name}\" of relation \"{relation}\""
            ),
        ));
    }
    if deferrability && !is_foreign_key {
        return Err(constraint_error(
            "42809",
            format!(
                "constraint \"{name}\" of relation \"{relation}\" is not a foreign key constraint"
            ),
        ));
    }
    if no_inherit && !matches!(location, ConstraintLocation::NotNull(_)) {
        return Err(constraint_error(
            "42809",
            format!(
                "constraint \"{name}\" of relation \"{relation}\" is not a not-null constraint"
            ),
        ));
    }
    Ok(())
}

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
        ancestor,
    } = options;
    // Diagnostics name the relation without its schema, as `RelationGetRelationName` does.
    let relation = uqa_core::RelationIdentity::from_legacy_name(table)
        .map_err(SQLError::Internal)?
        .name;
    let location = find_constraint(columns, constraints, name).ok_or_else(|| {
        constraint_error(
            "42704",
            format!("constraint \"{name}\" of relation \"{relation}\" does not exist"),
        )
    })?;
    ensure_alteration_applies(
        location,
        &relation,
        name,
        enforceability.is_some(),
        deferrability.is_some(),
        no_inherit.is_some(),
    )?;
    // A constraint derived on a referenced partition derives from the foreign key that holds it.
    let ancestor = match location {
        ConstraintLocation::ReferencedPartition(foreign_key, _) => foreign_key
            .derived(columns, constraints)
            .map(|(foreign_key, _)| ConstraintAncestor {
                name: foreign_key.to_string(),
                table: table.to_string(),
            }),
        _ => ancestor,
    };
    if let Some(ancestor) = &ancestor {
        return Err(derived_constraint_alteration(table, name, ancestor));
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
            | ConstraintLocation::Key(_)
            | ConstraintLocation::ReferencedPartition(..) => None,
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
            validate_after_publish =
                crate::schema::inheritance::foreign_keys::DeclaredForeignKey::Column(index).alter(
                    columns,
                    constraints,
                    enforceability,
                    deferrability,
                );
        }
        ConstraintLocation::TableForeignKey(index) => {
            validate_after_publish =
                crate::schema::inheritance::foreign_keys::DeclaredForeignKey::Table(index).alter(
                    columns,
                    constraints,
                    enforceability,
                    deferrability,
                );
        }
        ConstraintLocation::ColumnCheck(_)
        | ConstraintLocation::TableCheck(_)
        | ConstraintLocation::Key(_)
        | ConstraintLocation::ReferencedPartition(..) => {}
    }
    Ok(ConstraintAlterEffects {
        recreated_foreign_key,
        validate_after_publish,
    })
}
