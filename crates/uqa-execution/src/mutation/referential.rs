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
        lock_document_key_dependencies, lock_existing_document_foreign_key_dependencies,
        lock_existing_document_rewrite_foreign_key_dependencies,
        period::period_foreign_key_coverage,
    },
    errors::{dml_storage_error, missing_document_error},
    events::{ReferentialActionContext, ReferentialRewritePreparation},
    identity::integer_primary_key_doc_id,
    locking::{lock_mutation_row, lock_mutation_target, lock_physical_mutation_target},
    prepared::{PreparedDeleteAction, PreparedDocumentDelete, PreparedDocumentRewrite},
};
use crate::query::locking::context::update_lock_strength;
use std::collections::BTreeSet;
use uqa_core::{DocId, Value};
use uqa_sql::{
    ast::{ForeignKey, ForeignKeyAction},
    semantics::{
        foreign_keys::{
            foreign_key_comparison_types, foreign_key_lookup_values, foreign_key_relation_name,
            ForeignKeyComparison,
        },
        partition::partition_insert_target,
        referential::referrers_to_for_actions,
    },
    SQLError, SQLParam,
};
use uqa_storage::document_store::Document;
mod context;
pub use context::{ReferentialContext, ReferentialDeferrals};
mod actions;
mod delete;
mod references;
mod rewrite;
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
    },
    Delete {
        attempted_document: Document,
    },
}
