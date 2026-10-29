//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Distinct sets retain admitted exact identities; hashes select candidates and never establish equality.

use super::{normalization::Normalizer, AnalyzedGroupingSets};
use crate::{
    plan::QueryBlockPlan, routines::RoutineResolution, FunctionTypeResolver, RowSchema, SQLError,
    SQLParam, ScalarExpr,
};
use std::hash::{BuildHasher, Hasher};
use uqa_core::memory::{MemoryReservation, OwnedMap, Produced, ProductionControl, ProductionVec};

pub(crate) fn expression_identity_with_control(
    engine: &dyn FunctionTypeResolver,
    expression: &ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<u8>>, SQLError> {
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
    super::serialization::identity(&expression, control)
}

pub(crate) fn analyze_distinct_grouping_with_control(
    engine: &dyn RoutineResolution,
    block: &QueryBlockPlan,
    schema: &RowSchema,
    outer: Option<&RowSchema>,
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<Produced<AnalyzedGroupingSets>, SQLError> {
    control.check()?;
    let hashing = std::collections::hash_map::RandomState::new();
    let mut seen = OwnedMap::<(u64, usize), Identity>::new();
    let mut retained = ProductionVec::new(*control);
    retained.reserve(block.grouping_sets.len())?;
    for (index, set) in block.grouping_sets.iter().enumerate() {
        control.check()?;
        let mut keys = ProductionVec::new(*control);
        keys.reserve(set.len())?;
        for expression in set {
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
        uqa_core::ordering::sort_by_with_control(
            &mut identity.keys,
            &mut || control.check().map_err(SQLError::from),
            |left, right, _| Ok(uqa_core::compare_bytes_with_control(left, right, control)?),
        )?;
        let mut count = usize::from(!identity.keys.is_empty());
        for read in 1..identity.keys.len() {
            if !uqa_core::compare_bytes_with_control(
                &identity.keys[count - 1],
                &identity.keys[read],
                control,
            )?
            .is_eq()
            {
                identity.keys.swap(count, read);
                count += 1;
            }
        }
        identity.keys.truncate(count);
        let mut hash = hashing.build_hasher();
        for key in &identity.keys {
            hash.write_usize(key.len());
            for chunk in key.chunks(4096) {
                control.check()?;
                hash.write(chunk);
            }
        }
        let hash = hash.finish();
        let mut collision = 0;
        let mut duplicate = false;
        while let Some(previous) = seen.get(&(hash, collision)) {
            if previous.matches(&identity, control)? {
                duplicate = true;
                break;
            }
            collision += 1;
        }
        if !duplicate {
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
    fn matches(&self, other: &Self, control: &ProductionControl<'_>) -> Result<bool, SQLError> {
        if self.keys.len() != other.keys.len() {
            return Ok(false);
        }
        for (left, right) in self.keys.iter().zip(&other.keys) {
            if !uqa_core::compare_bytes_with_control(left, right, control)?.is_eq() {
                return Ok(false);
            }
        }
        Ok(true)
    }
}
