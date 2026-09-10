//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row, key, foreign-key, and referential-action validation.

pub(crate) use index_adapters::{index_key_values, index_predicate_accepts};
mod index_adapters;

use super::{Engine, SQLError};

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
