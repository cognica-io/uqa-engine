//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row, key, foreign-key, and referential-action validation.

pub(crate) use index_adapters::{index_key_values, index_predicate_accepts};
mod index_adapters;

use super::{DocId, Document, Engine, SQLError, SQLParam};
use uqa_sql::ast::TableKeyConstraint;

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

mod referencing;
pub(in crate::sql) use referencing::integer_primary_key_doc_id;
