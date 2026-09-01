//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Tuple acquisition and `EvalPlanQual` recheck execution.

use super::{
    collect_source_leaf_plans, collect_source_leaves, copy_recheck_source_row, merge_lock_wait,
    recheck_storage_names_match, Engine, LockAcquire, LockRows, LockStrength, LockWait,
    PhysicalOperator, PhysicalRow, ResolvedRowLock, RowSchema, SQLError,
};

/// One physical tuple this candidate row must lock. A self-join names the same tuple through several visible qualifiers; the tuple is locked once at the strongest requested strength while every qualifier stays known so each marked alias is pinned to the substituted image during a recheck.
struct LockCandidate {
    qualifiers: Vec<std::sync::Arc<str>>,
    /// Base-scan qualifiers that produced this tuple, so identity-source rechecks pin exactly the inner scans that emitted it.
    scan_qualifiers: Vec<std::sync::Arc<str>>,
    storage_name: std::sync::Arc<str>,
    doc_id: uqa_core::DocId,
    display_name: String,
    strength: LockStrength,
    wait: LockWait,
    identity_source: bool,
    foreign_waited: bool,
}

enum CandidateRecheckTarget {
    Unchanged,
    Missing,
    Changed(uqa_core::DocId),
}

enum CandidateRecheckAdvance {
    Retained,
    Missing,
}

impl LockRows<'_> {
    // Keep candidate acquisition separate from successor traversal so release optimization never merges their control-flow graphs.
    #[inline(never)]
    #[expect(
        clippy::too_many_lines,
        reason = "preserves lock target and recheck order"
    )]
    pub(super) fn lock_physical_row(
        &mut self,
        row: PhysicalRow,
    ) -> Result<Option<PhysicalRow>, SQLError> {
        let mut candidates: Vec<LockCandidate> = Vec::new();
        for target in &self.targets {
            for origin in row.lock_origins() {
                if !lock_origin_matches_target(origin, target) {
                    continue;
                }
                if let Some(candidate) = candidates.iter_mut().find(|candidate| {
                    candidate.storage_name == origin.storage_name
                        && candidate.doc_id == origin.doc_id
                }) {
                    candidate.strength = candidate.strength.max(target.strength);
                    candidate.wait = merge_lock_wait(candidate.wait, target.wait);
                    candidate.identity_source |= target.identity_source;
                    if !candidate.qualifiers.contains(&origin.qualifier) {
                        candidate
                            .qualifiers
                            .push(std::sync::Arc::clone(&origin.qualifier));
                    }
                    if !candidate.scan_qualifiers.contains(&origin.scan_qualifier) {
                        candidate
                            .scan_qualifiers
                            .push(std::sync::Arc::clone(&origin.scan_qualifier));
                    }
                    continue;
                }
                candidates.push(LockCandidate {
                    qualifiers: vec![std::sync::Arc::clone(&origin.qualifier)],
                    scan_qualifiers: vec![std::sync::Arc::clone(&origin.scan_qualifier)],
                    storage_name: std::sync::Arc::clone(&origin.storage_name),
                    doc_id: origin.doc_id,
                    display_name: target.display_name.clone(),
                    strength: target.strength,
                    wait: target.wait,
                    identity_source: target.identity_source,
                    foreign_waited: false,
                });
            }
        }

        // PostgreSQL 18 holds RowShare on every base relation whose tuples a locking query returns, including relations reached through views and derived tables, so TRUNCATE and destructive DDL wait for them.
        for candidate in &candidates {
            if !self.relation_locked.contains(&candidate.storage_name) {
                self.engine.lock_relation(
                    candidate.storage_name.as_ref(),
                    crate::row_locks::RelationLockMode::RowShare,
                )?;
                self.relation_locked
                    .insert(std::sync::Arc::clone(&candidate.storage_name));
            }
        }
        let mut acquired = Vec::new();
        let mut waited = false;
        for candidate in &mut candidates {
            match self.engine.lock_row(
                candidate.storage_name.as_ref(),
                candidate.doc_id,
                candidate.strength,
                candidate.wait,
                &candidate.display_name,
            ) {
                Ok(LockAcquire::Granted {
                    waited: lock_waited,
                    foreign_waited,
                    acquisition,
                }) => {
                    waited |= lock_waited;
                    candidate.foreign_waited = foreign_waited;
                    if let Some(acquisition) = acquisition {
                        acquired.push(acquisition);
                    }
                }
                Ok(LockAcquire::Skipped) => {
                    // PostgreSQL retains tuple locks acquired for earlier target relations even when a later target makes this joined row a SKIP LOCKED miss. They remain transaction-scoped just like locks acquired for rows rejected after an EvalPlanQual recheck.
                    return Ok(None);
                }
                Err(error) => {
                    rollback_row_acquisitions(self.engine, acquired);
                    return Err(error);
                }
            }
        }
        // Inside one process, change epochs identify candidates changed after the statement snapshot. Durable candidates also verify their latest committed image unconditionally: an external writer may already have exited by the time this row acquires its lock.
        let foreign_waited = candidates.iter().any(|candidate| candidate.foreign_waited);
        let durable_coordination = self
            .engine
            .row_lock_manager()
            .has_cross_process_coordination();
        let requires_recheck = self.engine.row_lock_change_requires_recheck()?
            || foreign_waited
            || durable_coordination;
        if let Some(cache) = self.retry_cache.as_deref().filter(|_| requires_recheck) {
            return self.recheck_changed_candidates(row, candidates, cache);
        }
        if waited {
            for candidate in &candidates {
                match self
                    .engine
                    .get_document(candidate.storage_name.as_ref(), candidate.doc_id)
                {
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        rollback_row_acquisitions(self.engine, acquired);
                        return Ok(None);
                    }
                    Err(error) => {
                        rollback_row_acquisitions(self.engine, acquired);
                        return Err(error);
                    }
                }
            }
        }
        Ok(Some(row))
    }

    /// `PostgreSQL` 18 `EvalPlanQual`: when a selected tuple was concurrently updated by a committed transaction whose mutation strength conflicts with the requested row lock, re-evaluate this candidate in place. The candidate keeps its original scan and sort position, its original join partners stay pinned, `LIMIT` membership is decided by the recheck outcome, and a primary-key rewrite is followed to the successor row.
    // Keep successor traversal separate from candidate acquisition and committed-image comparison.
    #[inline(never)]
    fn recheck_changed_candidates(
        &self,
        row: PhysicalRow,
        mut candidates: Vec<LockCandidate>,
        cache: &super::super::RowLockRetryCache,
    ) -> Result<Option<PhysicalRow>, SQLError> {
        let mut overrides: Vec<Option<super::super::RetryRowOverride>> = Vec::new();
        overrides.resize_with(candidates.len(), || None);
        // Once this session holds a candidate's tuple lock, no other transaction can commit a further conflicting change to it, so a candidate whose committed image was fetched after its lock was acquired is final. Only successor identities surfaced by a primary-key rewrite need another pass: their lock is taken below, after which their refetched image is final too.
        let mut handled = vec![false; candidates.len()];
        let mut visited_doc_ids = candidates
            .iter()
            .map(|candidate| std::collections::BTreeSet::from([candidate.doc_id]))
            .collect::<Vec<_>>();
        let mut any_changed = false;
        // Commits from other OS processes bypass the in-process change epochs, so every durable candidate verifies its latest committed image even after the writer process exits.
        let durable_coordination = self
            .engine
            .row_lock_manager()
            .has_cross_process_coordination();
        loop {
            let mut progressed = false;
            for (((candidate, row_override), handled), visited_doc_ids) in candidates
                .iter_mut()
                .zip(&mut overrides)
                .zip(&mut handled)
                .zip(&mut visited_doc_ids)
            {
                if *handled {
                    continue;
                }
                let recheck_target =
                    self.candidate_recheck_target(candidate, cache, durable_coordination)?;
                if self.engine.current_transaction_uses_fixed_snapshot()
                    && !matches!(recheck_target, CandidateRecheckTarget::Unchanged)
                {
                    return Err(crate::sql::dml::concurrent_update_serialization_failure());
                }
                let target_doc_id = match recheck_target {
                    CandidateRecheckTarget::Unchanged => {
                        *handled = true;
                        continue;
                    }
                    CandidateRecheckTarget::Missing => return Ok(None),
                    CandidateRecheckTarget::Changed(target_doc_id) => target_doc_id,
                };
                any_changed = true;
                progressed = true;
                if matches!(
                    self.apply_candidate_recheck(
                        candidate,
                        row_override,
                        handled,
                        visited_doc_ids,
                        cache,
                        target_doc_id,
                    )?,
                    CandidateRecheckAdvance::Missing
                ) {
                    return Ok(None);
                }
            }
            if !progressed {
                break;
            }
        }
        if !any_changed {
            return Ok(Some(row));
        }
        self.run_candidate_recheck(&row, &candidates, &overrides)
    }

    // Resolve one candidate's post-snapshot state without combining cache observation with successor locking.
    #[inline(never)]
    fn candidate_recheck_target(
        &self,
        candidate: &LockCandidate,
        cache: &super::super::RowLockRetryCache,
        durable_coordination: bool,
    ) -> Result<CandidateRecheckTarget, SQLError> {
        match cache.conflicting_change_target_since_snapshot(
            candidate.storage_name.as_ref(),
            candidate.doc_id,
            candidate.strength,
        )? {
            crate::row_locks::RowChangeTarget::Deleted => Ok(CandidateRecheckTarget::Missing),
            crate::row_locks::RowChangeTarget::Present(target_doc_id) => {
                Ok(CandidateRecheckTarget::Changed(target_doc_id))
            }
            crate::row_locks::RowChangeTarget::Unchanged => {
                if !(candidate.foreign_waited || durable_coordination) {
                    return Ok(CandidateRecheckTarget::Unchanged);
                }
                if self.cross_process_candidate_changed(candidate, cache)? {
                    Ok(CandidateRecheckTarget::Changed(candidate.doc_id))
                } else {
                    Ok(CandidateRecheckTarget::Unchanged)
                }
            }
        }
    }

    // Apply one committed image and, for a primary-key rewrite, lock the successor before the outer traversal retries it.
    #[inline(never)]
    fn apply_candidate_recheck(
        &self,
        candidate: &mut LockCandidate,
        row_override: &mut Option<super::super::RetryRowOverride>,
        handled: &mut bool,
        visited_doc_ids: &mut std::collections::BTreeSet<uqa_core::DocId>,
        cache: &super::super::RowLockRetryCache,
        target_doc_id: uqa_core::DocId,
    ) -> Result<CandidateRecheckAdvance, SQLError> {
        let committed = cache.committed_override(
            self.engine,
            candidate.storage_name.as_ref(),
            candidate.doc_id,
            target_doc_id,
            candidate.strength,
        )?;
        match committed {
            super::super::RetryRowOverride::Deleted => {
                // Lock targets are never on the nullable side of an active outer join, so a deleted tuple always eliminates this candidate row. Locks already acquired stay until transaction end, matching PostgreSQL's treatment of dead EPQ tuples.
                Ok(CandidateRecheckAdvance::Missing)
            }
            super::super::RetryRowOverride::Present { doc_id, .. } => {
                if doc_id == candidate.doc_id {
                    *row_override = Some(committed);
                    *handled = true;
                    return Ok(CandidateRecheckAdvance::Retained);
                }
                if !visited_doc_ids.insert(doc_id) {
                    return Err(SQLError::Internal(format!(
                        "row-lock successor chain for relation `{}` contains a cycle at document {doc_id}",
                        candidate.display_name
                    )));
                }
                // PostgreSQL follows the update chain to the row the blocker moved the tuple to, locks it, and rechecks that successor. The refetch on the next pass reads the successor after its lock is held, so a change committed while waiting here is still observed.
                match self.engine.lock_row(
                    candidate.storage_name.as_ref(),
                    doc_id,
                    candidate.strength,
                    candidate.wait,
                    &candidate.display_name,
                )? {
                    LockAcquire::Granted { .. } => {}
                    LockAcquire::Skipped => return Ok(CandidateRecheckAdvance::Missing),
                }
                *row_override = Some(committed);
                candidate.doc_id = doc_id;
                Ok(CandidateRecheckAdvance::Retained)
            }
        }
    }

    /// Whether another OS process committed a change to this candidate that conflicts with the requested lock strength. Foreign commits are invisible to the in-process change epochs, so the latest committed image is compared with the statement snapshot and the mutation strength is derived from the changed columns, exactly like the epochs derive it from the writer's own column set. A row this transaction already rewrote itself is authoritative as-is.
    // Keep committed-image comparison separate from the successor traversal state machine.
    #[inline(never)]
    fn cross_process_candidate_changed(
        &self,
        candidate: &LockCandidate,
        cache: &super::super::RowLockRetryCache,
    ) -> Result<bool, SQLError> {
        let table = candidate.storage_name.as_ref();
        if self
            .engine
            .row_changed_in_open_transaction(table, candidate.doc_id)?
        {
            return Ok(false);
        }
        let committed = cache.committed_override(
            self.engine,
            table,
            candidate.doc_id,
            candidate.doc_id,
            candidate.strength,
        )?;
        let committed_document = match &committed {
            // Primary-key rewrites from other processes were already followed through the sidecar journal by the caller, so a missing committed image here is a genuine delete; PostgreSQL 18 drops a candidate whose tuple was deleted.
            super::super::RetryRowOverride::Deleted => return Ok(true),
            super::super::RetryRowOverride::Present { document, .. } => document,
        };
        let Some(snapshot_document) = self.engine.get_document(table, candidate.doc_id)? else {
            return Ok(true);
        };
        let mut changed_columns = Vec::new();
        for (column, value) in committed_document {
            if snapshot_document.get(column) != Some(value) {
                changed_columns.push(column.clone());
            }
        }
        for column in snapshot_document.keys() {
            if !committed_document.contains_key(column) {
                changed_columns.push(column.clone());
            }
        }
        if changed_columns.is_empty() {
            return Ok(false);
        }
        let mutation_strength =
            crate::sql::dml::update_lock_strength(self.engine, table, &changed_columns);
        Ok(crate::row_locks::lock_strengths_conflict(
            mutation_strength,
            candidate.strength,
        ))
    }

    /// Re-execute the plan below this `LockRows` boundary with every base scan pinned to the tuple that formed the original candidate. Changed lock targets substitute their committed image; unmarked join partners keep their statement-snapshot image, matching `PostgreSQL`'s `EvalPlanQual` row marks.
    // This contention-only rebuild stays out of the per-row locking path.
    #[cold]
    #[inline(never)]
    #[expect(
        clippy::too_many_lines,
        reason = "preserves lock target and recheck order"
    )]
    fn run_candidate_recheck(
        &self,
        row: &PhysicalRow,
        candidates: &[LockCandidate],
        overrides: &[Option<super::super::RetryRowOverride>],
    ) -> Result<Option<PhysicalRow>, SQLError> {
        let Some(source) = self.recheck_source.as_ref() else {
            return Err(SQLError::Internal(
                "row-lock recheck attempted without a rebuildable plan source".into(),
            ));
        };
        let source_leaves = source
            .statement
            .from
            .as_ref()
            .map(|from| collect_source_leaves(from, false, &source.ctes))
            .transpose()?
            .unwrap_or_default();
        let mut pins = super::super::RowLockRecheckPins::new();
        if let Some(from) = source.statement.from.as_ref() {
            let mut leaf_plans = Vec::new();
            collect_source_leaf_plans(from, &mut Vec::new(), &mut leaf_plans);
            if leaf_plans.len() != source_leaves.len() {
                return Err(SQLError::Internal(format!(
                    "row-lock recheck found {} source paths for {} source leaves",
                    leaf_plans.len(),
                    source_leaves.len()
                )));
            }
            for ((path, source_plan), leaf) in leaf_plans.into_iter().zip(&source_leaves) {
                let is_lock_target = candidates.iter().any(|candidate| {
                    candidate
                        .qualifiers
                        .iter()
                        .any(|qualifier| qualifier.as_ref() == leaf.qualifier)
                });
                if is_lock_target {
                    continue;
                }
                let (schema, source_row) = copy_recheck_source_row(
                    self.engine,
                    source_plan,
                    &leaf.qualifier,
                    &self.schema,
                    row,
                    self.params,
                    &source.ctes,
                )?;
                pins.pin_source_row(path, leaf.qualifier.clone(), schema, source_row);
            }
        }
        for origin in row.lock_origins() {
            let changed_target = candidates.iter().zip(overrides).find(|(candidate, _)| {
                candidate.storage_name == origin.storage_name
                    && candidate.qualifiers.contains(&origin.qualifier)
                    && candidate.scan_qualifiers.contains(&origin.scan_qualifier)
            });
            let (doc_id, document) =
                changed_target.map_or((origin.doc_id, None), |(candidate, row_override)| {
                    let document = match row_override {
                        Some(super::super::RetryRowOverride::Present { document, .. }) => {
                            Some(std::sync::Arc::new(document.clone()))
                        }
                        _ => None,
                    };
                    (candidate.doc_id, document)
                });
            let identity_source = source_leaves
                .iter()
                .find(|leaf| leaf.qualifier == origin.qualifier.as_ref())
                .is_some_and(|leaf| leaf.kind.is_identity_source())
                || origin.qualifier != origin.scan_qualifier;
            pins.pin_target(
                origin.qualifier.as_ref(),
                origin.storage_name.as_ref(),
                origin.scan_qualifier.as_ref(),
                identity_source,
                vec![super::super::RecheckDoc { doc_id, document }],
            );
        }
        let mut recheck_ctes = source.ctes.clone();
        recheck_ctes.activate_row_lock_recheck(std::sync::Arc::new(pins));
        let mut operator = super::super::build_row_lock_recheck_operator(
            self.engine,
            &source.statement,
            self.params,
            &mut recheck_ctes,
            source.ordered,
            &source.projections,
        )?;
        operator = align_recheck_schema(operator, &self.schema)?;
        operator.open().map_err(super::super::physical_exec_error)?;
        let first_row = loop {
            match operator.next() {
                Ok(Some(batch)) => {
                    let schema = batch.schema;
                    if let Some(row) = batch.rows.into_iter().next() {
                        break Some(match schema.relayout_physical_row(row, &self.schema) {
                            Ok(row) => row,
                            Err(error) => {
                                return Err(super::super::close_after_physical_failure(
                                    operator.as_mut(),
                                    error,
                                    "row-lock recheck relayout",
                                ));
                            }
                        });
                    }
                }
                Ok(None) => break None,
                Err(error) => {
                    let _ = operator.close();
                    return Err(super::super::physical_exec_error(error));
                }
            }
        };
        operator
            .close()
            .map_err(super::super::physical_exec_error)?;
        Ok(first_row)
    }
}

fn rollback_row_acquisitions(
    engine: &Engine,
    acquisitions: Vec<crate::row_locks::RowLockAcquisition>,
) {
    for acquisition in acquisitions.into_iter().rev() {
        engine.rollback_row_lock_acquisition(acquisition);
    }
}

/// Align a rebuilt recheck pipeline with the original lock boundary schema. The single-relation access path derives its scan order from the pruned projection while the join builder uses catalog column order, so the same columns can arrive in a different physical order. Positions are resolved by column identity; any column the rebuild cannot supply is an error, not a silent divergence.
#[cold]
#[inline(never)]
fn align_recheck_schema<'a>(
    operator: Box<dyn PhysicalOperator + 'a>,
    expected: &RowSchema,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    let rebuilt = operator.row_schema();
    if rebuilt.columns() == expected.columns() {
        return Ok(operator);
    }
    let mut positions = Vec::with_capacity(expected.len());
    let mut used = vec![false; rebuilt.len()];
    for (index, column) in expected.columns().iter().enumerate() {
        let wanted_identity = expected.identities().get(index);
        let mut resolved = None;
        for (candidate, candidate_column) in rebuilt.columns().iter().enumerate() {
            if used[candidate] || candidate_column != column {
                continue;
            }
            let identity_matches = match (wanted_identity, rebuilt.identities().get(candidate)) {
                (Some(wanted), Some(candidate_identity)) => {
                    wanted.qualifier().is_none()
                        || candidate_identity.qualifier().is_none()
                        || wanted.qualifier() == candidate_identity.qualifier()
                }
                _ => true,
            };
            if !identity_matches {
                continue;
            }
            if resolved.is_some() {
                return Err(SQLError::Internal(format!(
                    "row-lock recheck column `{column}` is ambiguous in the rebuilt schema {:?}",
                    rebuilt.columns()
                )));
            }
            resolved = Some(candidate);
        }
        let Some(position) = resolved else {
            return Err(SQLError::Internal(format!(
                "row-lock recheck schema {:?} diverged from the lock boundary schema {:?}",
                rebuilt.columns(),
                expected.columns()
            )));
        };
        used[position] = true;
        positions.push((column.clone(), position));
    }
    Ok(Box::new(uqa_execution::ColumnSelection::with_positions(
        operator, positions,
    )))
}

fn lock_origin_matches_target(
    origin: &uqa_execution::RowLockOrigin,
    target: &ResolvedRowLock,
) -> bool {
    if origin.qualifier.as_ref() != target.qualifier {
        return false;
    }
    if target.identity_source {
        return true;
    }
    recheck_storage_names_match(origin.storage_name.as_ref(), &target.storage_name)
}
