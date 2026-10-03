//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepare referential actions and partition rewrites before publishing row changes.
use super::{
    assignment::refresh_stored_generated_columns,
    candidate::{MutationLockTarget, PhysicalDocumentIdentity, PhysicalMutationLockTarget},
    constraints::{
        lock_document_key_dependencies, partition_insert_target,
        period::period_foreign_key_coverage, validate_partition_constraint, ConstraintStatement,
    },
    errors::{dml_storage_error, missing_document_error},
    events::{ReferentialActionContext, ReferentialRewritePreparation},
    identity::{arriving_key_identity, key_relocation},
    locking::{lock_mutation_row, lock_mutation_target, lock_physical_mutation_target},
    prepared::{
        PreparedDeleteAction, PreparedDocumentDelete, PreparedDocumentRewrite, ReferentialRewrite,
    },
};
use crate::query::locking::context::update_lock_strength;
use std::collections::BTreeSet;
use uqa_core::{DocId, Value};
use uqa_sql::{
    ast::{ForeignKey, ForeignKeyAction},
    semantics::{
        foreign_keys::{
            foreign_key_comparison_types, foreign_key_lookup_values, ForeignKeyComparison,
        },
        referential::referrers_to_for_actions,
    },
    SQLError, SQLParam,
};
use uqa_storage::document_store::Document;
mod context;
pub use context::{
    ReferentialContext, ReferentialDeferrals, ReferentialReadSnapshot, ReferentialSnapshots,
};
mod actions;
pub mod checks;
mod delete;
mod references;
mod rewrite;
mod snapshots;
pub use actions::{prepare_referenced_key_delete_actions, prepare_referenced_key_update_actions};
pub use delete::prepare_document_delete;
pub use references::{
    apply_set_action_to_child, lock_referencing_child, referencing_rows, ReferencingChildLock,
};
pub use rewrite::{
    prepare_document_rewrite, prepare_partition_update_route, prepare_referential_document_rewrite,
    prepare_routed_document_rewrite, reject_partition_rewrite,
};
pub enum PartitionUpdateRoute {
    Rewrite {
        document: Document,
        destination: Option<String>,
        /// The relation the `UPDATE` names when the row moves to another partition, whose constraints a change to a referenced key fires, as `PostgreSQL` fires the update triggers of the update's root for a moved row.
        moved_through: Option<String>,
    },
    Delete {
        attempted_document: Document,
    },
}
