//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Tuple pins for `PostgreSQL`-style row-lock rechecks.
//!
//! When a candidate row's locked tuple changed while the statement waited, the recheck re-executes the plan below the `LockRows` boundary with every base relation pinned to the candidate's exact tuple, exactly like `PostgreSQL` 18's `EvalPlanQual`: marked relations substitute the latest committed image while unmarked join partners keep their original image.

use std::sync::Arc;

use uqa_execution::{PhysicalRow, RowSchema};
use uqa_storage::document_store::Document;

/// One tuple a pinned scan must emit during a recheck.
#[derive(Clone)]
pub(in crate::sql) struct RecheckDoc {
    /// Row identity the pinned scan emits, already following any primary-key rewrite to the successor id.
    pub doc_id: uqa_core::DocId,
    /// Latest committed image for a changed tuple; `None` reads the statement snapshot image for `doc_id`.
    pub document: Option<Arc<Document>>,
}

/// Pins for every lock-target relation of one candidate row.
pub(in crate::sql) struct RowLockRecheckPins {
    targets: Vec<TargetPins>,
    source_rows: Vec<SourceRowPin>,
}

struct TargetPins {
    qualifier: String,
    storage_name: String,
    /// Qualifier of the base scan these tuples belong to. For a direct table target it equals `qualifier`; for an identity-source target it names the inner scan inside the view or derived table.
    scan_qualifier: String,
    identity_source: bool,
    docs: Arc<Vec<RecheckDoc>>,
}

/// Exact output of one top-level FROM leaf that is not a row-lock target. `PostgreSQL` represents these as copy row marks during `EvalPlanQual`; keeping the positional row shares its physical fragments and preserves duplicate and volatile source values without named-row materialization.
#[derive(Clone)]
pub(in crate::sql) struct RecheckSourceRow {
    pub schema: RowSchema,
    pub row: PhysicalRow,
    pub qualifier: String,
}

struct SourceRowPin {
    path: Box<[u8]>,
    source: RecheckSourceRow,
}

impl RowLockRecheckPins {
    pub(in crate::sql) fn new() -> Self {
        Self {
            targets: Vec::new(),
            source_rows: Vec::new(),
        }
    }

    pub(in crate::sql) fn pin_source_row(
        &mut self,
        path: Vec<u8>,
        qualifier: String,
        schema: RowSchema,
        row: PhysicalRow,
    ) {
        self.source_rows.push(SourceRowPin {
            path: path.into_boxed_slice(),
            source: RecheckSourceRow {
                schema,
                row,
                qualifier,
            },
        });
    }

    pub(in crate::sql) fn source_row(&self, path: &[u8]) -> Option<RecheckSourceRow> {
        self.source_rows
            .iter()
            .find(|pin| pin.path.as_ref() == path)
            .map(|pin| pin.source.clone())
    }

    pub(in crate::sql) fn pin_target(
        &mut self,
        qualifier: &str,
        storage_name: &str,
        scan_qualifier: &str,
        identity_source: bool,
        docs: Vec<RecheckDoc>,
    ) {
        if let Some(target) = self.targets.iter_mut().find(|target| {
            target.qualifier == qualifier
                && target.storage_name == storage_name
                && target.scan_qualifier == scan_qualifier
                && target.identity_source == identity_source
        }) {
            for doc in docs {
                if !target
                    .docs
                    .iter()
                    .any(|existing| existing.doc_id == doc.doc_id)
                {
                    Arc::make_mut(&mut target.docs).push(doc);
                }
            }
            return;
        }
        self.targets.push(TargetPins {
            qualifier: qualifier.to_string(),
            storage_name: storage_name.to_string(),
            scan_qualifier: scan_qualifier.to_string(),
            identity_source,
            docs: Arc::new(docs),
        });
    }

    /// Pinned tuples for a base scan addressed by its own qualifier. Direct table targets match here; identity-source targets pin base scans through [`Self::storage_pins_for_identity_source`] instead because the origin qualifier was rebound at the derived-table boundary.
    pub(in crate::sql) fn docs_for_scan(
        &self,
        qualifier: &str,
        storage_name: &str,
    ) -> Option<Arc<Vec<RecheckDoc>>> {
        self.targets
            .iter()
            .find(|target| {
                !target.identity_source
                    && target.qualifier == qualifier
                    && recheck_storage_names_match(&target.storage_name, storage_name)
            })
            .map(|target| Arc::clone(&target.docs))
    }

    /// Scan-level pins for the subtree of one identity-source target (a view, derived table, or locked subquery visible as `qualifier`). Each entry names a base storage plus the inner scan qualifier whose emitted tuples it pins.
    pub(in crate::sql) fn storage_pins_for_identity_source(
        &self,
        qualifier: &str,
    ) -> Vec<(String, String, Arc<Vec<RecheckDoc>>)> {
        self.targets
            .iter()
            .filter(|target| target.identity_source && target.qualifier == qualifier)
            .map(|target| {
                (
                    target.storage_name.clone(),
                    target.scan_qualifier.clone(),
                    Arc::clone(&target.docs),
                )
            })
            .collect()
    }
}

pub(in crate::sql) fn recheck_storage_names_match(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }
    match (left.rsplit_once('.'), right.rsplit_once('.')) {
        (Some((_, left_local)), None) => left_local == right,
        (None, Some((_, right_local))) => left == right_local,
        (None, None) | (Some(_), Some(_)) => false,
    }
}
