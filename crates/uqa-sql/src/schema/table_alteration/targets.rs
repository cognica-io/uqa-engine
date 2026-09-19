//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve ALTER TABLE names before locking and lower the revalidated relation kind afterward.
use super::syntax::{
    alter_foreign_table_from_table_syntax, alter_sequence_from_table_syntax,
    alter_view_from_table_syntax,
};
use crate::{
    ast::{AlterForeignTableStmt, AlterSequence, AlterTableAction, AlterTableStmt, AlterViewStmt},
    catalog::resolution::{resolve_relation_rename_source, RelationResolution},
    schema::relation_alteration::RelationAlterTarget,
    SQLError,
};

pub enum BoundTableAlteration {
    Table(AlterTableStmt),
    Sequence(AlterSequence),
    View(AlterViewStmt),
    ForeignTable(AlterForeignTableStmt),
    ViewEvents {
        name: String,
        actions: Vec<AlterTableAction>,
    },
    ForeignTableEvents {
        name: String,
        actions: Vec<AlterTableAction>,
    },
}

pub fn table_alter_target(
    resolution: RelationResolution,
    statement: &AlterTableStmt,
    notice: &mut dyn FnMut(&str),
) -> Result<Option<RelationAlterTarget>, SQLError> {
    let Some((canonical, kind)) =
        resolve_relation_rename_source(resolution, &statement.table, statement.if_exists, notice)?
    else {
        return Ok(None);
    };
    RelationAlterTarget::from_name(canonical, kind).map(Some)
}

pub fn bind_table_alteration(
    target: RelationAlterTarget,
    mut statement: AlterTableStmt,
) -> Result<BoundTableAlteration, SQLError> {
    let RelationAlterTarget {
        canonical, kind, ..
    } = target;
    let bound = match kind {
        "table" => {
            statement.table = canonical;
            BoundTableAlteration::Table(statement)
        }
        "sequence" => BoundTableAlteration::Sequence(alter_sequence_from_table_syntax(
            &canonical, &statement,
        )?),
        "foreign table" => match alter_foreign_table_from_table_syntax(&canonical, &statement)? {
            Some(change) => BoundTableAlteration::ForeignTable(change),
            None => BoundTableAlteration::ForeignTableEvents {
                name: canonical,
                actions: statement.actions,
            },
        },
        "view" | "materialized view" => {
            match alter_view_from_table_syntax(&canonical, kind, &statement)? {
                Some(change) => BoundTableAlteration::View(change),
                None => BoundTableAlteration::ViewEvents {
                    name: canonical,
                    actions: statement.actions,
                },
            }
        }
        _ => {
            return Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!("ALTER TABLE: relation `{canonical}` is a {kind}, not a table"),
            });
        }
    };
    Ok(bound)
}

#[cfg(test)]
mod tests;
