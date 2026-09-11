//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! TRUNCATE authorization, trigger ordering and transaction-scoped table clearing.

use uqa_sql::{
    ast::{TriggerTiming, TruncateTarget},
    schema::truncate::{
        resolve_truncate_targets, truncate_dependency_order, validate_truncate_references,
        TruncateCatalog, TruncateTargets,
    },
    SQLError, SQLResult,
};

pub trait TruncateAccess {
    fn ensure_truncate_privilege(&self, table: &str) -> Result<(), SQLError>;
}
pub trait TruncateTriggers {
    fn ensure_no_pending_trigger_events(
        &self,
        table: &str,
        operation: &str,
    ) -> Result<(), SQLError>;
    fn fire_statement_trigger(&self, table: &str, timing: TriggerTiming) -> Result<(), SQLError>;
}
pub trait TruncateStorage {
    fn truncate_tables_with_identity(
        &self,
        tables: &[String],
        restart_identity: bool,
    ) -> Result<(), SQLError>;
}
pub type TruncateWrite<'a> = Box<dyn FnOnce(&TruncateContext<'_>) -> Result<(), SQLError> + 'a>;
pub trait TruncateTransactions {
    fn transaction_depth(&self) -> usize;
    fn with_transaction(&self, operation: TruncateWrite<'_>) -> Result<(), SQLError>;
}
pub struct TruncateContext<'a> {
    pub catalog: &'a dyn TruncateCatalog,
    pub access: &'a dyn TruncateAccess,
    pub triggers: &'a dyn TruncateTriggers,
    pub storage: &'a dyn TruncateStorage,
    pub transactions: &'a dyn TruncateTransactions,
}

#[cfg(test)]
mod tests;

pub fn execute(
    context: &TruncateContext<'_>,
    tables: &[TruncateTarget],
    cascade: bool,
    restart_identity: bool,
) -> Result<SQLResult, SQLError> {
    let targets = resolve_truncate_targets(context.catalog, tables, cascade)?;
    for table in &targets.privilege_targets {
        context.access.ensure_truncate_privilege(table)?;
    }
    for table in &targets.trigger_order {
        context
            .triggers
            .ensure_no_pending_trigger_events(table, "TRUNCATE")?;
    }
    if !cascade {
        validate_truncate_references(context.catalog, &targets)?;
    }
    if context.transactions.transaction_depth() == 0 {
        context.transactions.with_transaction(Box::new(|context| {
            run_truncate(context, &targets, restart_identity)
        }))?;
    } else {
        run_truncate(context, &targets, restart_identity)?;
    }
    Ok(SQLResult::empty())
}

fn run_truncate(
    context: &TruncateContext<'_>,
    targets: &TruncateTargets,
    restart_identity: bool,
) -> Result<(), SQLError> {
    for table in &targets.trigger_order {
        context
            .triggers
            .fire_statement_trigger(table, TriggerTiming::Before)?;
    }
    let ordered = truncate_dependency_order(context.catalog, targets)?;
    context
        .storage
        .truncate_tables_with_identity(&ordered, restart_identity)?;
    for table in &targets.trigger_order {
        context
            .triggers
            .fire_statement_trigger(table, TriggerTiming::After)?;
    }
    Ok(())
}
