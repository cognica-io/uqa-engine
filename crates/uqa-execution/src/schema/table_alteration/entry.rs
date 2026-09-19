//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute bound ALTER TABLE actions through the native relation transaction boundaries.
use super::{
    binding::{bind_table_alteration, TableAlterBindingContext},
    TableAlterContext, TableEventLifecycle,
};
use crate::schema::{
    foreign_table_alteration::{
        alter_foreign_table, ForeignTableAlterAccess, ForeignTableAlterTransactions,
    },
    sequences::entry::{run_alter_sequence, SequenceAlterTransactions},
    view_alteration::{alter_view, ViewAlterTransactions},
};
use uqa_sql::{
    ast::{AlterTableAction, AlterTableStmt},
    schema::table_alteration::{
        syntax::validate_alter_table_transaction, targets::BoundTableAlteration,
    },
    SQLError, SQLResult,
};

pub trait TableAlterSession {
    fn in_transaction_block(&self) -> bool;
}
pub type TableAlterWrite<'a, S> =
    Box<dyn FnOnce(&TableAlterContext<'_, S>) -> Result<SQLResult, SQLError> + 'a>;
pub trait TableAlterTransactions<S: Clone + 'static> {
    fn with_table_write(&self, write: TableAlterWrite<'_, S>) -> Result<SQLResult, SQLError>;
}

pub struct RelationEventAlterContext<'a> {
    pub events: &'a dyn TableEventLifecycle,
    pub foreign_access: &'a dyn ForeignTableAlterAccess,
}
pub type RelationEventAlterWrite<'a> =
    Box<dyn FnOnce(&RelationEventAlterContext<'_>) -> Result<SQLResult, SQLError> + 'a>;
pub trait RelationEventAlterTransactions {
    fn with_event_write(&self, write: RelationEventAlterWrite<'_>) -> Result<SQLResult, SQLError>;
}

pub struct TableAlterEntryContext<'a, S: Clone + 'static> {
    pub session: &'a dyn TableAlterSession,
    pub binding: TableAlterBindingContext<'a>,
    pub tables: &'a dyn TableAlterTransactions<S>,
    pub events: &'a dyn RelationEventAlterTransactions,
    pub views: &'a dyn ViewAlterTransactions,
    pub foreign_tables: &'a dyn ForeignTableAlterTransactions,
    pub sequences: &'a dyn SequenceAlterTransactions,
    pub notices: &'a parking_lot::Mutex<Vec<(String, String)>>,
}

pub fn run_alter_table<S: Clone + 'static>(
    context: &TableAlterEntryContext<'_, S>,
    statement: AlterTableStmt,
) -> Result<SQLResult, SQLError> {
    validate_alter_table_transaction(&statement, context.session.in_transaction_block())?;
    let Some(bound) = bind_table_alteration(&context.binding, statement)? else {
        return Ok(SQLResult::empty());
    };
    match bound {
        BoundTableAlteration::Table(statement) => {
            context.tables.with_table_write(Box::new(move |tables| {
                super::run_alter_table(tables, statement)
            }))
        }
        BoundTableAlteration::Sequence(statement) => {
            run_alter_sequence(context.sequences, context.notices, &statement)
        }
        BoundTableAlteration::View(statement) => {
            alter_view(context.views, &statement)?;
            Ok(SQLResult::empty())
        }
        BoundTableAlteration::ForeignTable(statement) => {
            alter_foreign_table(context.foreign_tables, &statement)?;
            Ok(SQLResult::empty())
        }
        BoundTableAlteration::ViewEvents { name, actions } => {
            context.events.with_event_write(Box::new(move |context| {
                for action in &actions {
                    match action {
                        AlterTableAction::RenameRule { from, to } => {
                            context.events.rename_rule(&name, from, to)?;
                        }
                        AlterTableAction::RenameTrigger { from, to } => {
                            context.events.rename_trigger(&name, from, to)?;
                        }
                        _ => unreachable!("view ALTER was restricted to event lifecycle actions"),
                    }
                }
                Ok(SQLResult::empty())
            }))
        }
        BoundTableAlteration::ForeignTableEvents { name, actions } => {
            context.events.with_event_write(Box::new(move |context| {
                context.foreign_access.ensure_owner(&name)?;
                for action in &actions {
                    match action {
                        AlterTableAction::RenameTrigger { from, to } => {
                            context.events.rename_trigger(&name, from, to)?;
                        }
                        AlterTableAction::SetTriggerEnableMode {
                            name: trigger,
                            mode,
                            ..
                        } => {
                            context.events.set_trigger_enable_mode(
                                &name,
                                trigger.as_deref(),
                                *mode,
                            )?;
                        }
                        _ => unreachable!("foreign-table trigger actions were checked above"),
                    }
                }
                Ok(SQLResult::empty())
            }))
        }
    }
}
