//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row, key, foreign-key, and referential-action validation.

pub(crate) use index_adapters::{index_key_values, index_predicate_accepts};
mod index_adapters;
mod staging;

use super::{
    DocId, Document, Engine, ForeignKey, PhysicalDocumentIdentity, PreparedDocumentRewrite,
    ReferentialActionContext, SQLError, SQLParam, Value,
};
use uqa_sql::ast::TableKeyConstraint;

pub(in crate::sql) fn period_foreign_key_coverage(
    engine: &Engine,
    foreign_key: &ForeignKey,
    local_values: &[Value],
    excluded_parents: &[PhysicalDocumentIdentity],
    replacement_parent: Option<(&PhysicalDocumentIdentity, &Document)>,
) -> Result<(bool, Vec<PhysicalDocumentIdentity>), SQLError> {
    uqa_execution::mutation::constraints::period::period_foreign_key_coverage(
        engine.constraint_execution_context(),
        foreign_key,
        local_values,
        excluded_parents,
        replacement_parent,
    )
}

pub(in crate::sql) use staging::stage_prepared_document_rewrite;

pub(in crate::sql) use uqa_sql::semantics::foreign_keys::foreign_key_relation_name;

pub(in crate::sql) use uqa_sql::semantics::foreign_keys::ForeignKeyLookup;

pub(in crate::sql) fn validate_document_constraints(
    engine: &Engine,
    table: &str,
    document: &Document,
    params: &[SQLParam],
    ignored_doc_id: Option<DocId>,
) -> Result<(), SQLError> {
    uqa_execution::mutation::constraints::validate_document_constraints(
        engine.constraint_execution_context(),
        table,
        document,
        params,
        ignored_doc_id,
    )
}

/// Acquire every referenced-parent tuple lock that already exists without rejecting a temporarily missing parent. INSERT uses this as a lock-only preflight for all input rows before taking the backend writer; ordinary constraint validation still runs in row order afterwards, so a self-referencing row can see a parent inserted earlier by the same statement and a genuinely missing parent still raises the normal error.
pub(in crate::sql) fn lock_existing_document_foreign_key_dependencies(
    engine: &Engine,
    table: &str,
    document: &Document,
) -> Result<(), SQLError> {
    uqa_execution::mutation::constraints::lock_existing_document_foreign_key_dependencies(
        engine.constraint_execution_context(),
        table,
        document,
    )
}

pub(crate) fn validate_deferred_foreign_key_checks(
    engine: &Engine,
    checks: &[crate::DeferredForeignKeyCheck],
    targets: Option<&std::collections::BTreeSet<crate::ConstraintIdentity>>,
) -> Result<(), SQLError> {
    uqa_execution::mutation::constraints::validate_deferred_foreign_key_checks(
        engine.constraint_execution_context(),
        checks,
        targets,
    )
}

pub(in crate::sql) fn without_overlaps_conflict(
    engine: &Engine,
    table: &str,
    constraint: &TableKeyConstraint,
    document: &Document,
    ignored_doc_id: Option<DocId>,
) -> Result<bool, SQLError> {
    uqa_execution::mutation::constraints::without_overlaps_conflict(
        engine.constraint_execution_context(),
        table,
        constraint,
        document,
        ignored_doc_id,
    )
}

/// Reserve every UNIQUE / PRIMARY KEY value that a new row can publish, or every such value changed by a rewrite, before the backend writer is held. The reservation is the logical equivalent of `PostgreSQL`'s speculative index-tuple wait: a deferred reader that cannot yet see another writer's uncommitted row waits on the exact key, refreshes its snapshot, and only then decides whether INSERT or ON CONFLICT applies.
pub(in crate::sql) fn lock_document_key_dependencies(
    engine: &Engine,
    table: &str,
    document: &Document,
    old_document: Option<&Document>,
) -> Result<Vec<crate::row_locks::RowLockAcquisition>, SQLError> {
    uqa_execution::mutation::constraints::lock_document_key_dependencies(
        engine.constraint_execution_context(),
        table,
        document,
        old_document,
    )
}

pub(in crate::sql) fn foreign_key_lookup_values(
    engine: &Engine,
    table: &str,
    fk: &ForeignKey,
    document: &Document,
) -> Result<Option<ForeignKeyLookup>, SQLError> {
    uqa_sql::semantics::foreign_keys::foreign_key_lookup_values(engine, table, fk, document)
}

pub(in crate::sql) fn find_foreign_key_parent(
    engine: &Engine,
    fk: &ForeignKey,
    lookup: &ForeignKeyLookup,
) -> Result<Option<PhysicalDocumentIdentity>, SQLError> {
    uqa_execution::mutation::constraints::find_foreign_key_parent(
        engine.constraint_execution_context(),
        fk,
        lookup,
    )
}

pub(in crate::sql) use uqa_execution::mutation::referential::PartitionUpdateRoute;

mod referencing;
mod rewrite;
pub(in crate::sql) use referencing::integer_primary_key_doc_id;
pub(in crate::sql) use rewrite::{prepare_partition_update_route, prepare_routed_document_rewrite};
