//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! How `MergeAttributes` merges a new table's columns with the ones its parents give it: a column that two parents share through `MergeInheritedAttribute`, a declared column that a parent's column takes through `MergeChildAttribute`, and the defaults that parents give one column differently.

use crate::ast::{ColumnDef, ColumnType, Expr, GeneratedColumnKind};
use crate::{SQLError, SQLNotice};

/// `MaxHeapAttributeNumber`, the most columns a table can have.
const MAX_TABLE_COLUMNS: usize = 1600;

/// Reject more columns than a table can have.
pub(super) fn check_column_count(count: usize) -> Result<(), SQLError> {
    if count > MAX_TABLE_COLUMNS {
        return Err(routine(
            "54011",
            format!("tables can have at most {MAX_TABLE_COLUMNS} columns"),
        ));
    }
    Ok(())
}

/// Reject a column that the statement declares twice.
pub(super) fn reject_repeated_columns(columns: &[ColumnDef]) -> Result<(), SQLError> {
    for (position, column) in columns.iter().enumerate() {
        if columns[position + 1..]
            .iter()
            .any(|later| later.name == column.name)
        {
            return Err(routine(
                "42701",
                format!("column \"{}\" specified more than once", column.name),
            ));
        }
    }
    Ok(())
}

/// The columns parents give a new table, in parent order, and the ones whose parents give them different defaults or generation expressions.
#[derive(Default)]
pub(super) struct InheritedColumns {
    pub(super) columns: Vec<ColumnDef>,
    conflicting_defaults: Vec<String>,
}

impl InheritedColumns {
    /// `MergeInheritedAttribute`: a parent's column joins the columns earlier parents gave, merging with the column of its name, whose type and generation it must share; a default unlike an earlier parent's is a conflict that the new table must resolve.
    pub(super) fn merge_parent_column(
        &mut self,
        column: ColumnDef,
        notices: &mut Vec<SQLNotice>,
    ) -> Result<(), SQLError> {
        let Some(existing) = self
            .columns
            .iter_mut()
            .find(|existing| existing.name == column.name)
        else {
            self.columns.push(column);
            return Ok(());
        };
        notices.push(SQLNotice::notice(format!(
            "merging multiple inherited definitions of column \"{}\"",
            column.name
        )));
        if existing.ty != column.ty {
            return Err(type_conflict(
                format!("inherited column \"{}\" has a type conflict", column.name),
                &existing.ty,
                &column.ty,
            ));
        }
        merge_not_null(existing, &column);
        if generation_kind(existing) != generation_kind(&column) {
            return Err(routine(
                "42804",
                format!(
                    "inherited column \"{}\" has a generation conflict",
                    column.name
                ),
            ));
        }
        let conflicts = match (default_expression(existing), default_expression(&column)) {
            (Some(left), Some(right)) => left != right,
            _ => false,
        };
        if default_expression(existing).is_none() {
            existing.default = column.default;
            existing.generated = column.generated;
        }
        if conflicts && !self.conflicting_defaults.contains(&column.name) {
            self.conflicting_defaults.push(column.name);
        }
        Ok(())
    }

    /// `MergeChildAttribute`: a declared column takes the place of the parents' column of its name, with a notice that says whether it moved there; it must share the column's type, cannot specify a default or identity for a generated column or a generation expression for one that is not, and its default or generation expression replaces the parents'.
    pub(super) fn merge_declared_column(
        &mut self,
        position: usize,
        mut column: ColumnDef,
        notices: &mut Vec<SQLNotice>,
    ) -> Result<(), SQLError> {
        let Some(index) = self
            .columns
            .iter()
            .position(|existing| existing.name == column.name)
        else {
            self.columns.push(column);
            return Ok(());
        };
        notices.push(if index == position {
            SQLNotice::notice(format!(
                "merging column \"{}\" with inherited definition",
                column.name
            ))
        } else {
            SQLNotice::notice(format!(
                "moving and merging column \"{}\" with inherited definition",
                column.name
            ))
            .with_detail("User-specified column moved to the position of the inherited column.")
        });
        let inherited = &mut self.columns[index];
        if inherited.ty != column.ty {
            return Err(type_conflict(
                format!("column \"{}\" has a type conflict", column.name),
                &inherited.ty,
                &column.ty,
            ));
        }
        check_generation_merge(inherited, &column)?;
        if column.auto_increment.is_some() {
            // Identity is never inherited by an inheritance child; the declared column's identity applies.
            inherited.auto_increment.clone_from(&column.auto_increment);
        }
        merge_local_not_null(inherited, &column);
        // A declared default or generation expression replaces the parents', and settles their conflict.
        if column.default.is_some() || column.generated.is_some() {
            if column.generated.is_some() {
                inherited.generated = column.generated.take();
            } else {
                inherited.default = column.default.take();
            }
            self.conflicting_defaults
                .retain(|name| *name != column.name);
        }
        adopt_declared_constraints(inherited, column);
        Ok(())
    }

    /// `MergeAttributes` for a partition's column options: each names a column of the parent, which keeps its type and generation under the rules of `MergeChildAttribute`, and takes the option's default or generation expression, NOT NULL and constraints; no notice reports the merge.
    pub(super) fn merge_partition_option(&mut self, mut column: ColumnDef) -> Result<(), SQLError> {
        let Some(index) = self
            .columns
            .iter()
            .position(|existing| existing.name == column.name)
        else {
            return Err(routine(
                "42703",
                format!("column \"{}\" does not exist", column.name),
            ));
        };
        let inherited = &mut self.columns[index];
        check_generation_merge(inherited, &column)?;
        merge_local_not_null(inherited, &column);
        if column.generated.is_some() {
            inherited.generated = column.generated.take();
        } else if column.default.is_some() {
            inherited.default = column.default.take();
        }
        adopt_declared_constraints(inherited, column);
        Ok(())
    }

    /// The columns whose parents gave different defaults that the new table does not replace, as `MergeAttributes` reports them last.
    pub(super) fn reject_conflicting_defaults(&self) -> Result<(), SQLError> {
        for column in &self.columns {
            if !self.conflicting_defaults.contains(&column.name) {
                continue;
            }
            return Err(if column.generated.is_some() {
                SQLError::Diagnostic {
                    sqlstate: "42611".into(),
                    message: format!(
                        "column \"{}\" inherits conflicting generation expressions",
                        column.name
                    ),
                    detail: None,
                    hint: Some(
                        "To resolve the conflict, specify a generation expression explicitly."
                            .into(),
                    ),
                }
            } else {
                SQLError::Diagnostic {
                    sqlstate: "42611".into(),
                    message: format!(
                        "column \"{}\" inherits conflicting default values",
                        column.name
                    ),
                    detail: None,
                    hint: Some("To resolve the conflict, specify a default explicitly.".into()),
                }
            });
        }
        Ok(())
    }
}

/// The generation rules of `MergeChildAttribute`: a generated parent column takes no declared default or identity, a parent column that is not generated takes no generation expression, and a generated column keeps its parent's kind.
fn check_generation_merge(inherited: &ColumnDef, column: &ColumnDef) -> Result<(), SQLError> {
    if inherited.generated.is_some() {
        if column.default.is_some() && column.generated.is_none() {
            return Err(routine(
                "42611",
                format!(
                    "column \"{}\" inherits from generated column but specifies default",
                    column.name
                ),
            ));
        }
        if column.auto_increment.is_some() {
            return Err(routine(
                "42611",
                format!(
                    "column \"{}\" inherits from generated column but specifies identity",
                    column.name
                ),
            ));
        }
    } else if column.generated.is_some() {
        return Err(SQLError::Diagnostic {
            sqlstate: "42611".into(),
            message: format!(
                "child column \"{}\" specifies generation expression",
                column.name
            ),
            detail: None,
            hint: Some(
                "A child table column cannot be generated unless its parent column is.".into(),
            ),
        });
    }
    match (generation_kind(inherited), generation_kind(column)) {
        (Some(parent), Some(child)) if parent != child => Err(SQLError::Diagnostic {
            sqlstate: "42611".into(),
            message: format!(
                "column \"{}\" inherits from generated column of different kind",
                column.name
            ),
            detail: Some(format!(
                "Parent column is {}, child column is {}.",
                kind_name(parent),
                kind_name(child)
            )),
            hint: None,
        }),
        _ => Ok(()),
    }
}

/// The CHECK, key and reference a declared column writes, which the merged column keeps.
fn adopt_declared_constraints(inherited: &mut ColumnDef, column: ColumnDef) {
    inherited.primary_key |= column.primary_key;
    inherited.unique |= column.unique;
    if column.check.is_some() {
        inherited.check = column.check;
        inherited.check_name = column.check_name;
        inherited.check_enforced = column.check_enforced;
        inherited.check_validated = column.check_validated;
        inherited.check_no_inherit = column.check_no_inherit;
        inherited.check_is_local = column.check_is_local;
        inherited.check_object_id = column.check_object_id;
    }
    if column.references.is_some() {
        inherited.references = column.references;
    }
}

fn default_expression(column: &ColumnDef) -> Option<&Expr> {
    column
        .generated
        .as_ref()
        .map(|generated| generated.expression.as_ref())
        .or(column.default.as_ref())
}

fn generation_kind(column: &ColumnDef) -> Option<GeneratedColumnKind> {
    column.generated.as_ref().map(|generated| generated.kind)
}

fn merge_not_null(existing: &mut ColumnDef, column: &ColumnDef) {
    if column.not_null && !existing.not_null {
        existing.not_null_name.clone_from(&column.not_null_name);
        existing.not_null_identity = column.not_null_identity;
        existing.not_null_validated = column.not_null_validated;
    }
    existing.not_null |= column.not_null;
}

/// A declared NOT NULL constraint stands for the inherited one, keeping its own name, validation and inheritance.
fn merge_local_not_null(inherited: &mut ColumnDef, declared: &ColumnDef) {
    let is_local = (inherited.not_null && inherited.not_null_is_local)
        || (declared.not_null && declared.not_null_is_local);
    if declared.not_null && (!inherited.not_null || declared.not_null_is_local) {
        inherited.not_null_name.clone_from(&declared.not_null_name);
        inherited.not_null_identity = declared.not_null_identity;
        inherited.not_null_validated = declared.not_null_validated;
        inherited.not_null_no_inherit = declared.not_null_no_inherit;
    }
    inherited.not_null |= declared.not_null;
    inherited.not_null_is_local = !inherited.not_null || is_local;
    inherited.not_null_explicit |= declared.not_null_explicit;
}

const fn kind_name(kind: GeneratedColumnKind) -> &'static str {
    match kind {
        GeneratedColumnKind::Stored => "STORED",
        GeneratedColumnKind::Virtual => "VIRTUAL",
    }
}

fn type_conflict(message: String, inherited: &ColumnType, declared: &ColumnType) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: "42804".into(),
        message,
        detail: Some(format!(
            "{} versus {}",
            format_type_with_typemod(inherited),
            format_type_with_typemod(declared)
        )),
        hint: None,
    }
}

/// `format_type_with_typemod`: a built-in type with its modifiers, and a user-defined type as the search path finds it.
fn format_type_with_typemod(ty: &ColumnType) -> String {
    if ty.user_type_identity().is_some() {
        ty.regtype_name()
    } else {
        ty.sql_name()
    }
}

fn routine(sqlstate: &str, message: String) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message,
    }
}
