//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Distinct sets retain admitted exact identities; hashes select candidates and never establish equality.

use super::{normalization::Normalizer, AnalyzedGroupingSets};
use crate::catalog::work::{compare_bytes, inline, retire_entries, ReferenceWork, Work};
use crate::{
    plan::QueryBlockPlan, routines::RoutineResolution, FunctionTypeResolver, RowSchema, SQLError,
    SQLParam, ScalarExpr,
};
use std::hash::{BuildHasher, Hasher};
use uqa_core::memory::{MemoryReservation, OwnedMap, Produced, ProductionControl, ProductionVec};
use uqa_core::ordering::{sort_by_with_progress, SortProgress};

/// Resolved columns compare by physical slot. Other normalized expressions retain their exact serialized identity.
#[derive(Debug, PartialEq)]
pub(crate) enum GroupingKey {
    Position(usize),
    Expression(Produced<Vec<u8>>),
}

impl GroupingKey {
    pub(super) fn new(
        engine: &dyn FunctionTypeResolver,
        expression: &ScalarExpr,
        schema: &RowSchema,
        params: &[SQLParam],
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        if let Some(slot) = super::normalization::column_slot(schema, expression) {
            return Ok(Self::Position(slot));
        }
        let expression = crate::type_resolution::bind_type_introspection_owned(
            expression.copy_for_rewrite(control)?,
            schema,
            params,
            Some(engine),
            control,
        )?;
        let expression = Normalizer {
            engine,
            schema,
            params,
            control: *control,
        }
        .normalize(expression)?;
        match &*expression {
            ScalarExpr::Position(slot) => Ok(Self::Position(*slot)),
            _ => super::serialization::identity(&expression, control).map(Self::Expression),
        }
    }
}

pub(super) fn expression_identity_with_control(
    engine: &dyn FunctionTypeResolver,
    expression: &ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<u8>>, SQLError> {
    match GroupingKey::new(engine, expression, schema, params, control)? {
        GroupingKey::Position(slot) => {
            super::serialization::identity(&ScalarExpr::Position(slot), control)
        }
        GroupingKey::Expression(bytes) => Ok(bytes),
    }
}

pub(crate) fn analyze_distinct_grouping_with_control(
    engine: &dyn RoutineResolution,
    block: &QueryBlockPlan,
    schema: &RowSchema,
    outer: Option<&RowSchema>,
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<Produced<AnalyzedGroupingSets>, SQLError> {
    inline(analyze_distinct_grouping_from(
        engine,
        block,
        schema,
        outer,
        params,
        &mut ReferenceWork {
            control,
            work: &mut (),
        },
    ))
}

pub(crate) async fn analyze_distinct_grouping_from<W: Work + Send>(
    engine: &dyn RoutineResolution,
    block: &QueryBlockPlan,
    schema: &RowSchema,
    outer: Option<&RowSchema>,
    params: &[SQLParam],
    input: &mut ReferenceWork<'_, '_, W>,
) -> Result<Produced<AnalyzedGroupingSets>, SQLError> {
    let control = input.control;
    control.check()?;
    let hashing = std::collections::hash_map::RandomState::new();
    let mut seen = OwnedMap::<(u64, usize), Identity>::new();
    let mut retained = ProductionVec::new(*control);
    retained.reserve(block.grouping_sets.len())?;
    for (index, set) in block.grouping_sets.iter().enumerate() {
        input.checkpoint().await?;
        let mut keys = ProductionVec::new(*control);
        keys.reserve(set.len())?;
        for expression in set {
            input.checkpoint().await?;
            let expression = super::resolve_grouping_expression_reference_with_control(
                engine,
                expression,
                &block.projections,
                schema,
                outer,
                params,
                control,
            )?;
            let key =
                expression_identity_with_control(engine, expression, schema, params, control)?;
            keys.push_produced(control.finish(key, control.empty_reservation())?)?;
        }
        let (keys, memory) = keys.finish()?.into_parts();
        let mut identity = Identity {
            keys,
            _memory: memory,
        };
        sort_by_with_progress(&mut identity.keys, input).await?;
        let mut count = usize::from(!identity.keys.is_empty());
        for read in 1..identity.keys.len() {
            if !input
                .compare(&identity.keys[count - 1], &identity.keys[read])
                .await?
                .is_eq()
            {
                identity.keys.swap(count, read);
                count += 1;
            }
        }
        retire_entries(identity.keys.drain(count..), control, input.work).await?;
        let mut hash = hashing.build_hasher();
        for key in &identity.keys {
            hash.write_usize(key.len());
            for chunk in key.chunks(4096) {
                input.checkpoint().await?;
                hash.write(chunk);
            }
        }
        let hash = hash.finish();
        let mut collision = 0;
        let mut duplicate = false;
        while let Some(previous) = seen.get(&(hash, collision)) {
            if previous.matches(&identity, input).await? {
                duplicate = true;
                break;
            }
            collision += 1;
        }
        if duplicate {
            retire_entries(identity.keys.into_iter(), control, input.work).await?;
        } else {
            match control.budget() {
                Some(budget) => {
                    seen.insert_budgeted((hash, collision), identity, budget)?;
                }
                None => {
                    seen.insert((hash, collision), identity);
                }
            }
            retained.push_copy(index)?;
        }
    }
    for (_, identity) in seen {
        input.checkpoint().await?;
        retire_entries(identity.keys.into_iter(), control, input.work).await?;
    }
    let (retained, memory) = retained.finish()?.into_parts();
    Ok(control.finish(
        AnalyzedGroupingSets {
            original_count: block.grouping_sets.len(),
            retained,
        },
        memory,
    )?)
}

struct Identity {
    keys: Vec<Produced<Vec<u8>>>,
    _memory: Option<MemoryReservation>,
}
impl Identity {
    async fn matches<W: Work + Send>(
        &self,
        other: &Self,
        input: &mut ReferenceWork<'_, '_, W>,
    ) -> Result<bool, SQLError> {
        if self.keys.len() != other.keys.len() {
            return Ok(false);
        }
        for (left, right) in self.keys.iter().zip(&other.keys) {
            if !input.compare(left, right).await?.is_eq() {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

impl<W: Work + Send> SortProgress<Produced<Vec<u8>>, SQLError> for ReferenceWork<'_, '_, W> {
    async fn checkpoint(&mut self) -> Result<(), SQLError> {
        Self::checkpoint(self).await
    }
    async fn compare(
        &mut self,
        left: &Produced<Vec<u8>>,
        right: &Produced<Vec<u8>>,
    ) -> Result<std::cmp::Ordering, SQLError> {
        compare_bytes(self.work, left, right, self.control).await
    }
}
