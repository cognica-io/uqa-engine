//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The AFTER trigger events of one statement, queued and fired as `PostgreSQL`'s `trigger.c` queues and fires the events of one query level.
//!
//! Every command of a statement queues into one queue: the primary command, its data-modifying WITH items, and the referential actions that the queued events run as they fire. A relation and an operation that the statement writes keep one state (`AfterTriggersTableData`): the rows its transition tables collect, whether its BEFORE STATEMENT triggers fired, and where its AFTER STATEMENT events wait in the queue. A later command on the same relation and operation therefore fires no second BEFORE STATEMENT trigger and moves the AFTER STATEMENT events to the end of the queue (`cancel_prior_stmt_triggers`), until a trigger reads the transition tables and closes the state, after which the rows start a new one.

use super::context::TriggerContext;
use super::{AfterRowTriggerEvent, TransitionTables};
use std::collections::BTreeSet;
use std::sync::Arc;
use uqa_core::Value;
use uqa_sql::ast::{TriggerEvent, TriggerTiming};
use uqa_sql::error::Result;
use uqa_sql::SQLError;

/// A relation and operation that a command writes, with the columns an UPDATE sets, which its statement triggers fire for.
#[derive(Debug, Clone)]
pub struct StatementEvent {
    pub relation: String,
    pub event: TriggerEvent,
    pub columns: Vec<String>,
}

impl StatementEvent {
    pub fn new(relation: &str, event: TriggerEvent, columns: &[String]) -> Self {
        Self {
            relation: relation.to_string(),
            event,
            columns: columns.to_vec(),
        }
    }
}

/// The queued AFTER events of one statement.
#[derive(Default)]
pub struct AfterTriggerQueue {
    state: parking_lot::Mutex<QueueState>,
}

#[derive(Default)]
struct QueueState {
    entries: Vec<QueueEntry>,
    /// The first entry that has not fired.
    next: usize,
    tables: Vec<TableState>,
}

enum QueueEntry {
    Row {
        event: AfterRowTriggerEvent,
        /// The state whose transition tables collected the row.
        table: Option<usize>,
    },
    Statement {
        statement: StatementEvent,
        table: usize,
    },
    /// An entry that fired or that a later command cancelled.
    Done,
}

/// The state of one relation and operation within the statement.
struct TableState {
    relation: String,
    event: TriggerEvent,
    /// The relation and its partitions, whose rows the transition tables collect.
    sources: BTreeSet<String>,
    capture: TransitionCapture,
    /// A trigger has read the transition tables, which take no further rows.
    closed: bool,
    before_fired: bool,
    /// Where the AFTER STATEMENT events of the state were queued, from which a later command cancels them.
    statements_from: Option<usize>,
    old_rows: Vec<Value>,
    new_rows: Vec<Value>,
    transitions: Option<Arc<TransitionTables>>,
}

/// Which images of its rows a relation's triggers read as transition tables.
#[derive(Clone, Copy, Default)]
struct TransitionCapture {
    old: bool,
    new: bool,
}

impl QueueState {
    /// The open state of `relation` and `event`, created when every earlier one was closed (`GetAfterTriggersTableData`).
    fn open_table(
        &mut self,
        context: &TriggerContext<'_>,
        relation: &str,
        event: TriggerEvent,
    ) -> Result<usize> {
        let relation = context
            .relations
            .try_resolve_table_name(relation)
            .map_err(|error| SQLError::Internal(format!("resolve trigger relation: {error}")))?
            .unwrap_or_else(|| relation.to_string());
        if let Some(index) = self
            .tables
            .iter()
            .position(|table| !table.closed && table.event == event && table.relation == relation)
        {
            return Ok(index);
        }
        let mut capture = TransitionCapture::default();
        for row in [false, true] {
            for trigger in
                context
                    .catalog
                    .triggers_for(&relation, TriggerTiming::After, event, row, &[])?
            {
                capture.old |= trigger.definition.old_transition_table().is_some();
                capture.new |= trigger.definition.new_transition_table().is_some();
            }
        }
        // Only a relation whose triggers read transition tables collects rows, and a view has no partitions to collect them from.
        let sources = if capture.old || capture.new {
            context
                .catalog
                .hierarchy_scan_tables(&relation, true)?
                .into_iter()
                .collect()
        } else {
            BTreeSet::new()
        };
        self.tables.push(TableState {
            relation,
            event,
            sources,
            capture,
            closed: false,
            before_fired: false,
            statements_from: None,
            old_rows: Vec::new(),
            new_rows: Vec::new(),
            transitions: None,
        });
        Ok(self.tables.len() - 1)
    }

    /// Cancel the AFTER STATEMENT events that an earlier command queued for `table` and that have not fired.
    fn cancel_prior_statements(&mut self, table: usize) {
        let Some(from) = self.tables[table].statements_from else {
            return;
        };
        for entry in self.entries.iter_mut().skip(from.max(self.next)) {
            if matches!(entry, QueueEntry::Statement { table: queued, .. } if *queued == table) {
                *entry = QueueEntry::Done;
            }
        }
    }

    /// Take the next entry to fire, leaving it done.
    fn take_next(&mut self) -> Option<QueueEntry> {
        while self.next < self.entries.len() {
            let entry = std::mem::replace(&mut self.entries[self.next], QueueEntry::Done);
            self.next += 1;
            if !matches!(entry, QueueEntry::Done) {
                return Some(entry);
            }
        }
        None
    }
}

impl AfterTriggerQueue {
    /// Fire the BEFORE STATEMENT triggers of `statement` unless the statement already fired them for its open state (`before_stmt_triggers_fired`).
    pub fn fire_before_statement(
        &self,
        context: &TriggerContext<'_>,
        statement: &StatementEvent,
    ) -> Result<()> {
        let fire = {
            let mut state = self.state.lock();
            let table = state.open_table(context, &statement.relation, statement.event)?;
            !std::mem::replace(&mut state.tables[table].before_fired, true)
        };
        if fire {
            super::fire_statement_triggers(
                context,
                &statement.relation,
                TriggerTiming::Before,
                statement.event,
                &statement.columns,
            )?;
        }
        Ok(())
    }

    /// Queue the AFTER events of one command: the events of its rows, whose images join the transition tables of the statement event they belong to, then an AFTER STATEMENT event for each of `statements`, which replaces the one an earlier command queued for the same state.
    pub fn queue_command(
        &self,
        context: &TriggerContext<'_>,
        statements: &[StatementEvent],
        rows: Vec<AfterRowTriggerEvent>,
    ) -> Result<()> {
        let mut state = self.state.lock();
        let tables = statements
            .iter()
            .map(|statement| state.open_table(context, &statement.relation, statement.event))
            .collect::<Result<Vec<_>>>()?;
        for event in rows {
            let table = if event.captured {
                statements
                    .iter()
                    .zip(&tables)
                    .find(|(statement, table)| {
                        statement.event == event.event
                            && state.tables[**table].sources.contains(&event.table)
                    })
                    .map(|(_, table)| *table)
            } else {
                None
            };
            if let Some(table) = table {
                let target = &mut state.tables[table];
                if target.capture.old && !matches!(event.old, Value::Null) {
                    target.old_rows.push(event.old.clone());
                }
                if target.capture.new && !matches!(event.new, Value::Null) {
                    target.new_rows.push(event.new.clone());
                }
            }
            state.entries.push(QueueEntry::Row { event, table });
        }
        for (statement, table) in statements.iter().zip(tables) {
            state.cancel_prior_statements(table);
            let position = state.entries.len();
            state.tables[table].statements_from = Some(position);
            state.entries.push(QueueEntry::Statement {
                statement: statement.clone(),
                table,
            });
        }
        Ok(())
    }

    /// Fire the queued events in order, with the events that the referential actions among them queue as they run (`afterTriggerInvokeEvents`).
    pub fn fire(&self, context: &TriggerContext<'_>) -> Result<()> {
        loop {
            let Some(entry) = self.state.lock().take_next() else {
                return Ok(());
            };
            match entry {
                QueueEntry::Row { event, table } => {
                    super::fire_after_row_trigger(context, &event, table, self)?;
                }
                QueueEntry::Statement { statement, table } => {
                    self.fire_after_statement(context, &statement, table)?;
                }
                QueueEntry::Done => {}
            }
        }
    }

    fn fire_after_statement(
        &self,
        context: &TriggerContext<'_>,
        statement: &StatementEvent,
        table: usize,
    ) -> Result<()> {
        let triggers = context.catalog.triggers_for(
            &statement.relation,
            TriggerTiming::After,
            statement.event,
            false,
            &statement.columns,
        )?;
        if triggers.is_empty() {
            return Ok(());
        }
        let transitions = if triggers
            .iter()
            .any(|trigger| !trigger.definition.transition_relations.is_empty())
        {
            Some(self.transitions(context, table)?)
        } else {
            None
        };
        super::fire_statement_triggers_with_transition(
            context,
            &statement.relation,
            TriggerTiming::After,
            statement.event,
            &statement.columns,
            transitions.as_deref(),
        )
    }

    /// The transition tables of `table`, which reading closes to further rows.
    pub(super) fn transitions(
        &self,
        context: &TriggerContext<'_>,
        table: usize,
    ) -> Result<Arc<TransitionTables>> {
        let mut state = self.state.lock();
        let target = &mut state.tables[table];
        target.closed = true;
        if let Some(transitions) = &target.transitions {
            return Ok(Arc::clone(transitions));
        }
        let old = target
            .capture
            .old
            .then(|| {
                super::transitions::materialize_transition_rows(
                    context,
                    &target.relation,
                    std::mem::take(&mut target.old_rows),
                )
            })
            .transpose()?;
        let new = target
            .capture
            .new
            .then(|| {
                super::transitions::materialize_transition_rows(
                    context,
                    &target.relation,
                    std::mem::take(&mut target.new_rows),
                )
            })
            .transpose()?;
        let transitions = Arc::new(TransitionTables { old, new });
        target.transitions = Some(Arc::clone(&transitions));
        Ok(transitions)
    }
}
