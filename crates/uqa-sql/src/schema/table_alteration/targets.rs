//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind ALTER TABLE targets to native relation actions before execution enters a transaction.
use super::syntax::{
    alter_foreign_table_from_table_syntax, alter_sequence_from_table_syntax,
    alter_view_from_table_syntax,
};
use crate::{
    ast::{AlterForeignTableStmt, AlterSequence, AlterTableAction, AlterTableStmt, AlterViewStmt},
    catalog::resolution::{resolve_relation_rename_source, RelationResolution},
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

pub fn bind_table_alteration(
    resolution: RelationResolution,
    mut statement: AlterTableStmt,
    notice: &mut dyn FnMut(&str),
) -> Result<Option<BoundTableAlteration>, SQLError> {
    let resolution = if matches!(
        statement.actions.as_slice(),
        [AlterTableAction::RenameTable { .. }]
    ) {
        let Some(resolution) = resolve_relation_rename_source(
            resolution,
            &statement.table,
            statement.if_exists,
            notice,
        )?
        else {
            return Ok(None);
        };
        Some(resolution)
    } else {
        resolution.into_found()
    };
    let bound = match resolution {
        Some((canonical, "table")) => {
            statement.table = canonical;
            BoundTableAlteration::Table(statement)
        }
        Some((canonical, "sequence")) => BoundTableAlteration::Sequence(
            alter_sequence_from_table_syntax(&canonical, &statement)?,
        ),
        Some((canonical, "foreign table")) => {
            match alter_foreign_table_from_table_syntax(&canonical, &statement)? {
                Some(change) => BoundTableAlteration::ForeignTable(change),
                None => BoundTableAlteration::ForeignTableEvents {
                    name: canonical,
                    actions: statement.actions,
                },
            }
        }
        Some((canonical, kind @ ("view" | "materialized view"))) => {
            match alter_view_from_table_syntax(&canonical, kind, &statement)? {
                Some(change) => BoundTableAlteration::View(change),
                None => BoundTableAlteration::ViewEvents {
                    name: canonical,
                    actions: statement.actions,
                },
            }
        }
        Some((canonical, kind)) => {
            return Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!("ALTER TABLE: relation `{canonical}` is a {kind}, not a table"),
            });
        }
        None if statement.if_exists => {
            notice(&format!(
                "relation \"{}\" does not exist, skipping",
                statement.table
            ));
            return Ok(None);
        }
        None => {
            return Err(SQLError::Unsupported(format!(
                "ALTER TABLE: relation `{}` does not exist",
                statement.table
            )));
        }
    };
    Ok(Some(bound))
}
