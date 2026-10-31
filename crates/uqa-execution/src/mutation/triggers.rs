//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `BEFORE`/`AFTER`, row-level, and statement-level trigger execution.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use uqa_core::{DocId, Value};
use uqa_sql::ast::{TriggerEvent, TriggerTiming};
use uqa_sql::error::Result;
use uqa_sql::plpgsql::{bind_expr, ResolvedVariable, VariableResolver};
use uqa_sql::SQLError;
use uqa_storage::document_store::Document;

use context::TriggerContext;

#[cfg(test)]
mod tests;
use uqa_core::RelationIdentity;
pub mod context;

use crate::routines::TriggerRoutineContext;

type TransitionCaptureKey = (String, &'static str, Vec<String>);
type TransitionCaptureStack = Vec<BTreeMap<TransitionCaptureKey, bool>>;

thread_local! {
    static ACTIVE_TRANSITION_RELATIONS: RefCell<Vec<BTreeMap<String, crate::SharedSpill>>> = const { RefCell::new(Vec::new()) };
    static TRANSITION_CAPTURE_CACHE: RefCell<TransitionCaptureStack> = const { RefCell::new(Vec::new()) };
    /// How many trigger functions this thread is inside.
    static TRIGGER_DEPTH: std::cell::Cell<i64> = const { std::cell::Cell::new(0) };
}

/// How many trigger functions the running code is inside, as `pg_trigger_depth()` reports it: 0 outside any trigger.
pub fn trigger_depth() -> i64 {
    TRIGGER_DEPTH.with(std::cell::Cell::get)
}

/// One trigger function running until the guard drops.
struct TriggerDepthScope;

impl TriggerDepthScope {
    fn enter() -> Self {
        TRIGGER_DEPTH.with(|depth| depth.set(depth.get() + 1));
        Self
    }
}

impl Drop for TriggerDepthScope {
    fn drop(&mut self) {
        TRIGGER_DEPTH.with(|depth| depth.set(depth.get() - 1));
    }
}

pub struct TransitionRelationScope;

impl TransitionRelationScope {
    fn enter(relations: BTreeMap<String, crate::SharedSpill>) -> Self {
        ACTIVE_TRANSITION_RELATIONS.with(|active| active.borrow_mut().push(relations));
        Self
    }

    fn empty() -> Self {
        Self::enter(BTreeMap::new())
    }
}

impl Drop for TransitionRelationScope {
    fn drop(&mut self) {
        ACTIVE_TRANSITION_RELATIONS.with(|relations| {
            let removed = relations.borrow_mut().pop();
            debug_assert!(
                removed.is_some(),
                "transition relation scope stack underflow"
            );
        });
    }
}

pub fn current_transition_relations() -> BTreeMap<String, crate::SharedSpill> {
    ACTIVE_TRANSITION_RELATIONS
        .with(|relations| relations.borrow().last().cloned().unwrap_or_default())
}

/// Borrow the current trigger's immutable materializations while the caller captures its own bindings.
pub fn with_current_transition_relations<T>(
    visit: impl FnOnce(Option<&BTreeMap<String, crate::SharedSpill>>) -> T,
) -> T {
    ACTIVE_TRANSITION_RELATIONS.with(|relations| {
        let relations = relations.borrow();
        visit(relations.last())
    })
}

pub fn current_transition_relation_names() -> BTreeSet<String> {
    ACTIVE_TRANSITION_RELATIONS.with(|relations| {
        relations
            .borrow()
            .last()
            .map(|relations| relations.keys().cloned().collect())
            .unwrap_or_default()
    })
}

/// Capture owned transition names while the TLS map is borrowed; recursive SQL runs only after this snapshot returns.
pub fn current_transition_relation_names_with_control(
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<uqa_sql::binding::production::BindingNameSet> {
    control.check()?;
    with_current_transition_relations(|relations| {
        let mut names = uqa_sql::binding::production::BindingNameSet::new();
        if let Some(relations) = relations {
            for name in relations.keys() {
                names.insert_with_control(name, control)?;
            }
        }
        Ok(names)
    })
}

pub fn enter_empty_transition_relation_scope() -> TransitionRelationScope {
    TransitionRelationScope::empty()
}

/// A statement that writes rows. What it resolves about the triggers of its tables, which row triggers they have and whether their transitions are captured, holds until it ends.
pub struct TriggerStatementScope;

impl TriggerStatementScope {
    pub fn enter() -> Self {
        TRANSITION_CAPTURE_CACHE.with(|cache| cache.borrow_mut().push(BTreeMap::new()));
        row_triggers::enter();
        Self
    }
}

impl Drop for TriggerStatementScope {
    fn drop(&mut self) {
        row_triggers::leave();
        TRANSITION_CAPTURE_CACHE.with(|cache| {
            let removed = cache.borrow_mut().pop();
            debug_assert!(
                removed.is_some(),
                "transition capture cache stack underflow"
            );
        });
    }
}

pub mod events;
pub mod queue;
mod transitions;
pub use transitions::{transition_capture_required, TransitionTables};

mod row_triggers;
mod rows;
use rows::{
    trigger_column_metadata, trigger_document, trigger_record, TriggerColumnMetadata,
    TriggerVariableResolver,
};

fn operation_name(event: TriggerEvent) -> &'static str {
    match event {
        TriggerEvent::Insert => "INSERT",
        TriggerEvent::Update => "UPDATE",
        TriggerEvent::Delete => "DELETE",
        TriggerEvent::Truncate => "TRUNCATE",
    }
}

fn timing_name(timing: TriggerTiming) -> &'static str {
    match timing {
        TriggerTiming::Before => "BEFORE",
        TriggerTiming::After => "AFTER",
        TriggerTiming::InsteadOf => "INSTEAD OF",
    }
}

fn trigger_condition_matches(
    context: &TriggerContext<'_>,
    condition: Option<&uqa_sql::ast::Expr>,
    old: &Value,
    new: &Value,
    columns: &BTreeMap<String, TriggerColumnMetadata>,
) -> Result<bool> {
    let Some(condition) = condition else {
        return Ok(true);
    };
    let condition = bind_expr(
        condition,
        &mut TriggerVariableResolver { old, new, columns },
    )?;
    Ok(uqa_sql::expr::truthy(
        &context.expressions.evaluate(&condition)?,
    ))
}

struct TriggerInvocation<'a> {
    table: &'a str,
    trigger: &'a uqa_sql::catalog::events::StoredTrigger,
    timing: TriggerTiming,
    event: TriggerEvent,
    row: bool,
    old: Value,
    new: Value,
}

fn invoke_trigger(
    context: &TriggerContext<'_>,
    invocation: TriggerInvocation<'_>,
    transition_tables: Option<&TransitionTables>,
) -> Result<Value> {
    let relation = RelationIdentity::from_legacy_name(invocation.table).map_err(|error| {
        SQLError::Internal(format!(
            "decode trigger relation `{}`: {error}",
            invocation.table
        ))
    })?;
    let function = context.routines.resolve_bound_trigger_function(
        &invocation.trigger.definition.function,
        invocation.trigger.function_object_id,
    )?;
    let _transition_scope = match transition_tables {
        Some(tables) => tables.enter(&invocation.trigger.definition)?,
        None if invocation
            .trigger
            .definition
            .transition_relations
            .is_empty() =>
        {
            TransitionRelationScope::empty()
        }
        None => {
            return Err(SQLError::Internal(format!(
                "trigger `{}` requested unavailable transition tables",
                invocation.trigger.definition.name
            )))
        }
    };
    let _depth = TriggerDepthScope::enter();
    context.routines.execute_trigger_routine(
        &function,
        &TriggerRoutineContext {
            row_schema: uqa_sql::RowSchema::with_column_metadata(
                None,
                context
                    .catalog
                    .trigger_relation_definitions(invocation.table)?
                    .into_iter()
                    .map(|column| (column.name, Some(column.ty), column.collation_oid)),
            ),
            old: invocation.old,
            new: invocation.new,
            name: invocation.trigger.definition.name.clone(),
            when: timing_name(invocation.timing).into(),
            level: if invocation.row { "ROW" } else { "STATEMENT" }.into(),
            operation: operation_name(invocation.event).into(),
            relation_oid: crate::catalog::projection::event_relation_oid(
                &context.projection,
                invocation.table,
            )?,
            table_name: relation.name,
            table_schema: relation.schema,
            arguments: invocation.trigger.definition.arguments.clone(),
        },
    )
}

fn positional_trigger_record(
    columns: &[(String, uqa_sql::ast::ColumnType)],
    values: Option<&[Value]>,
) -> Result<Value> {
    if values.is_some_and(|values| values.len() != columns.len()) {
        return Err(SQLError::Internal(
            "INSTEAD OF trigger row does not match the view row type".into(),
        ));
    }
    Ok(Value::Record(
        columns
            .iter()
            .enumerate()
            .map(|(position, (name, _))| {
                (
                    name.clone(),
                    values
                        .and_then(|values| values.get(position))
                        .cloned()
                        .unwrap_or(Value::Null),
                )
            })
            .collect::<Vec<_>>()
            .into(),
    ))
}

fn normalize_instead_of_trigger_record(
    columns: &[(String, uqa_sql::ast::ColumnType)],
    value: Value,
) -> Result<Option<Vec<Value>>> {
    let fields = match value {
        Value::Null => return Ok(None),
        Value::Record(fields) => fields.into_iter().collect::<BTreeMap<_, _>>(),
        _ => {
            return Err(SQLError::Routine {
                sqlstate: "39P01".into(),
                message: "trigger function returned non-composite value".into(),
            })
        }
    };
    if let Some(unknown) = fields
        .keys()
        .find(|name| !columns.iter().any(|(column, _)| column == *name))
    {
        return Err(SQLError::UnknownColumn(unknown.clone()));
    }
    // The routine result has already passed descriptor validation; these values are admitted, not new input.
    Ok(Some(
        columns
            .iter()
            .map(|(name, _)| fields.get(name).cloned().unwrap_or(Value::Null))
            .collect(),
    ))
}

/// Run the `INSTEAD OF` row triggers of `view` for one row in name order, as `ExecIRInsertTriggers`, `ExecIRUpdateTriggers` and `ExecIRDeleteTriggers` do, and return the row the command then counts, checks and returns: the last trigger's result for INSERT and UPDATE and the original OLD row for DELETE, or `None` when a trigger returns NULL. When `session_replication_role` suppresses every trigger, the row is counted as it stands, although nothing performs the command.
pub fn fire_instead_of_row_triggers(
    context: &TriggerContext<'_>,
    view: &str,
    event: TriggerEvent,
    old_values: Option<&[Value]>,
    new_values: Option<&[Value]>,
    updated_columns: &[String],
) -> Result<Option<Vec<Value>>> {
    let columns = context.catalog.rule_relation_columns(view)?;
    let old = positional_trigger_record(&columns, old_values)?;
    let mut new = positional_trigger_record(&columns, new_values)?;
    let triggers = context.catalog.triggers_for(
        view,
        TriggerTiming::InsteadOf,
        event,
        true,
        updated_columns,
    )?;
    if triggers.is_empty()
        && !context
            .catalog
            .has_trigger_definition(view, TriggerTiming::InsteadOf, event, true)?
    {
        return Err(SQLError::Routine {
            sqlstate: "55000".into(),
            message: format!(
                "cannot {} view \"{}\": no active INSTEAD OF trigger",
                operation_name(event).to_ascii_lowercase(),
                RelationIdentity::from_legacy_name(view)
                    .map_or_else(|_| view.to_string(), |relation| relation.name)
            ),
        });
    }
    for trigger in triggers {
        let returned = invoke_trigger(
            context,
            TriggerInvocation {
                table: view,
                trigger: &trigger,
                timing: TriggerTiming::InsteadOf,
                event,
                row: true,
                old: old.clone(),
                new: new.clone(),
            },
            None,
        )?;
        let Some(values) = normalize_instead_of_trigger_record(&columns, returned)? else {
            return Ok(None);
        };
        if event != TriggerEvent::Delete {
            new = positional_trigger_record(&columns, Some(&values))?;
        }
    }
    let final_record = if event == TriggerEvent::Delete {
        old
    } else {
        new
    };
    normalize_instead_of_trigger_record(&columns, final_record)
}

pub fn fire_statement_triggers(
    context: &TriggerContext<'_>,
    table: &str,
    timing: TriggerTiming,
    event: TriggerEvent,
    updated_columns: &[String],
) -> Result<()> {
    fire_statement_triggers_with_transition(context, table, timing, event, updated_columns, None)
}

fn fire_statement_triggers_with_transition(
    context: &TriggerContext<'_>,
    table: &str,
    timing: TriggerTiming,
    event: TriggerEvent,
    updated_columns: &[String],
    transition_tables: Option<&TransitionTables>,
) -> Result<()> {
    for trigger in context
        .catalog
        .triggers_for(table, timing, event, false, updated_columns)?
    {
        let _ = invoke_trigger(
            context,
            TriggerInvocation {
                table,
                trigger: &trigger,
                timing,
                event,
                row: false,
                old: Value::Null,
                new: Value::Null,
            },
            transition_tables,
        )?;
    }
    Ok(())
}

/// Whether `table` has BEFORE ROW triggers for `event`, whatever replication role fires them, which make `PostgreSQL` fetch a row through `GetTupleForTrigger` before it updates or deletes it.
pub fn has_before_row_triggers(
    context: &TriggerContext<'_>,
    table: &str,
    event: TriggerEvent,
) -> Result<bool> {
    Ok(!row_triggers::resolve(context, table, TriggerTiming::Before, event, None)?.is_empty())
}

pub fn fire_before_row_triggers(
    context: &TriggerContext<'_>,
    table: &str,
    event: TriggerEvent,
    doc_id: DocId,
    old_document: Option<&Document>,
    new_document: Option<&Document>,
    updated_columns: &[String],
) -> Result<Option<Document>> {
    let triggers = row_triggers::resolve(
        context,
        table,
        TriggerTiming::Before,
        event,
        Some(updated_columns),
    )?;
    let original = if event == TriggerEvent::Delete {
        old_document
    } else {
        new_document
    };
    if !triggers
        .iter()
        .any(|trigger| row_triggers::fires(context, trigger))
    {
        return Ok(original.cloned());
    }
    let columns = trigger_column_metadata(context.catalog.trigger_relation_definitions(table)?);
    let old = trigger_record(context, table, doc_id, old_document, false)?;
    let mut new = trigger_record(context, table, doc_id, new_document, true)?;
    let mut invoked = false;
    for trigger in triggers.iter() {
        // An earlier trigger of this row may have changed the role.
        if !row_triggers::fires(context, trigger)
            || !trigger_condition_matches(
                context,
                trigger.definition.when.as_ref(),
                &old,
                &new,
                &columns,
            )?
        {
            continue;
        }
        invoked = true;
        let returned = invoke_trigger(
            context,
            TriggerInvocation {
                table,
                trigger,
                timing: TriggerTiming::Before,
                event,
                row: true,
                old: old.clone(),
                new: new.clone(),
            },
            None,
        )?;
        if matches!(returned, Value::Null) {
            return Ok(None);
        }
        if event != TriggerEvent::Delete {
            new = returned;
        }
    }
    if event == TriggerEvent::Delete || !invoked {
        return Ok(original.cloned());
    }
    trigger_document(context, table, new)
}

/// Fire the AFTER ROW triggers of one queued row, and run its foreign key checks and referential actions, which are internal triggers that fire among the user triggers in the order of their names. A referential action queues the events of the rows it writes in `queue`. `transition_table` is the state whose transition tables collected the row, which the first trigger that reads them closes, after the internal triggers that precede it have run.
fn fire_after_row_trigger(
    context: &TriggerContext<'_>,
    event: &AfterRowTriggerEvent,
    transition_table: Option<usize>,
    queue: &queue::AfterTriggerQueue,
) -> Result<()> {
    let mut checks = event.foreign_keys.iter().peekable();
    for trigger in &event.triggers {
        while let Some(check) =
            checks.next_if(|check| check.precedes_trigger(&trigger.definition.name))
        {
            context.foreign_keys.run_foreign_key_check(check, queue)?;
        }
        if trigger.definition.constraint
            && context.deferrals.constraint_trigger_is_deferred(trigger)?
        {
            context
                .deferrals
                .defer_constraint_trigger_event(DeferredConstraintTriggerEvent {
                    constraint: trigger.constraint_identity()?,
                    firing_relation: RelationIdentity::from_legacy_name(&event.table).map_err(
                        |error| {
                            SQLError::Internal(format!(
                                "decode deferred trigger relation `{}`: {error}",
                                event.table
                            ))
                        },
                    )?,
                    table: event.table.clone(),
                    event: event.event,
                    old: event.old.clone(),
                    new: event.new.clone(),
                    trigger: trigger.clone(),
                })?;
            continue;
        }
        let transition_tables = match transition_table {
            Some(table) if !trigger.definition.transition_relations.is_empty() => {
                Some(queue.transitions(context, table)?)
            }
            _ => None,
        };
        let _ = invoke_trigger(
            context,
            TriggerInvocation {
                table: &event.table,
                trigger,
                timing: TriggerTiming::After,
                event: event.event,
                row: true,
                old: event.old.clone(),
                new: event.new.clone(),
            },
            transition_tables.as_deref(),
        )?;
    }
    for check in checks {
        context.foreign_keys.run_foreign_key_check(check, queue)?;
    }
    Ok(())
}

#[derive(Clone)]
pub struct DeferredConstraintTriggerEvent {
    pub constraint: uqa_sql::catalog::constraints::ConstraintIdentity,
    pub firing_relation: RelationIdentity,
    pub table: String,
    event: TriggerEvent,
    old: Value,
    new: Value,
    pub trigger: uqa_sql::catalog::events::StoredTrigger,
}

pub fn fire_deferred_constraint_trigger_event(
    context: &TriggerContext<'_>,
    event: &DeferredConstraintTriggerEvent,
) -> Result<()> {
    let _ = invoke_trigger(
        context,
        TriggerInvocation {
            table: &event.table,
            trigger: &event.trigger,
            timing: TriggerTiming::After,
            event: event.event,
            row: true,
            old: event.old.clone(),
            new: event.new.clone(),
        },
        None,
    )?;
    Ok(())
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct AfterRowTriggerEvent {
    table: String,
    event: TriggerEvent,
    #[serde(with = "events::values")]
    old: Value,
    #[serde(with = "events::values")]
    new: Value,
    /// Whether `old` and `new` hold the row's images, which a row with no user trigger to fire and no transition to capture leaves out.
    captured: bool,
    triggers: Vec<uqa_sql::catalog::events::StoredTrigger>,
    /// The foreign key checks and referential actions the row queued, in the order of their internal triggers' names.
    foreign_keys: Vec<crate::mutation::referential::checks::ForeignKeyCheck>,
}

pub struct AfterRowTriggerInput<'a> {
    pub table: &'a str,
    pub event: TriggerEvent,
    pub old_doc_id: DocId,
    pub new_doc_id: DocId,
    pub old_document: Option<&'a Document>,
    pub new_document: Option<&'a Document>,
    pub updated_columns: &'a [String],
    /// The foreign key checks and referential actions the row's change queues (`referential::checks`).
    pub foreign_key_checks: Vec<crate::mutation::referential::checks::ForeignKeyCheck>,
}

impl AfterRowTriggerEvent {
    pub fn prepare(
        context: &TriggerContext<'_>,
        input: AfterRowTriggerInput<'_>,
    ) -> Result<Option<Self>> {
        let AfterRowTriggerInput {
            table,
            event,
            old_doc_id,
            new_doc_id,
            old_document,
            new_document,
            updated_columns,
            mut foreign_key_checks,
        } = input;
        crate::mutation::referential::checks::ForeignKeyCheck::sort(&mut foreign_key_checks);
        let candidates = row_triggers::resolve(
            context,
            table,
            TriggerTiming::After,
            event,
            Some(updated_columns),
        )?;
        let capture_transition =
            transition_capture_required(context, table, event, updated_columns)?;
        // The role is read when the row's events are queued, as `PostgreSQL` checks each trigger in `AfterTriggerSaveEvent`.
        if !capture_transition
            && !candidates
                .iter()
                .any(|trigger| row_triggers::fires(context, trigger))
        {
            return Ok(Self::foreign_key_checks(table, event, foreign_key_checks));
        }
        let columns = trigger_column_metadata(context.catalog.trigger_relation_definitions(table)?);
        let old = trigger_record(context, table, old_doc_id, old_document, false)?;
        let new = trigger_record(context, table, new_doc_id, new_document, false)?;
        let mut matching = Vec::new();
        for trigger in candidates.iter() {
            if row_triggers::fires(context, trigger)
                && trigger_condition_matches(
                    context,
                    trigger.definition.when.as_ref(),
                    &old,
                    &new,
                    &columns,
                )?
            {
                let mut captured = trigger.clone();
                // WHEN has already selected this event; firing must not retain or evaluate it again.
                captured.definition.when = None;
                matching.push(captured);
            }
        }
        if matching.is_empty() && !capture_transition {
            return Ok(Self::foreign_key_checks(table, event, foreign_key_checks));
        }
        Ok(Some(Self {
            table: table.to_string(),
            event,
            old,
            new,
            captured: true,
            triggers: matching,
            foreign_keys: foreign_key_checks,
        }))
    }

    /// An event that only checks foreign keys and takes referential actions, for a row with no user trigger to fire and no transition to capture; `None` when the row queued neither.
    pub fn foreign_key_checks(
        table: &str,
        event: TriggerEvent,
        mut foreign_key_checks: Vec<crate::mutation::referential::checks::ForeignKeyCheck>,
    ) -> Option<Self> {
        if foreign_key_checks.is_empty() {
            return None;
        }
        crate::mutation::referential::checks::ForeignKeyCheck::sort(&mut foreign_key_checks);
        Some(Self {
            table: table.to_string(),
            event,
            old: Value::Null,
            new: Value::Null,
            captured: false,
            triggers: Vec::new(),
            foreign_keys: foreign_key_checks,
        })
    }

    pub fn prepare_transition_capture(
        context: &TriggerContext<'_>,
        input: AfterRowTriggerInput<'_>,
    ) -> Result<Option<Self>> {
        let AfterRowTriggerInput {
            table,
            event,
            old_doc_id,
            new_doc_id,
            old_document,
            new_document,
            updated_columns,
            foreign_key_checks,
        } = input;
        if !transition_capture_required(context, table, event, updated_columns)? {
            return Ok(Self::foreign_key_checks(table, event, foreign_key_checks));
        }
        let old = match old_document {
            Some(document) => trigger_record(context, table, old_doc_id, Some(document), false)?,
            None => Value::Null,
        };
        let new = match new_document {
            Some(document) => trigger_record(context, table, new_doc_id, Some(document), false)?,
            None => Value::Null,
        };
        let mut foreign_key_checks = foreign_key_checks;
        crate::mutation::referential::checks::ForeignKeyCheck::sort(&mut foreign_key_checks);
        Ok(Some(Self {
            table: table.to_string(),
            event,
            old,
            new,
            captured: true,
            triggers: Vec::new(),
            foreign_keys: foreign_key_checks,
        }))
    }
}
