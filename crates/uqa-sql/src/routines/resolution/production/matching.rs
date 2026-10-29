//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native overload candidates retain their own admitted signature and invocation workspace.

use super::super::{RoutineCallKind, RoutineTypeSnapshot};
use super::{invocation, parameters};
use crate::{
    ast::RoutineInvocationBinding,
    routines::SQLUserFunction,
    type_resolution::{
        match_routine_candidate_with_control, MatchedRoutineSignature, RankedFunctionMatch,
        RoutineCallDescriptor, RoutineSignatureProductionError, RoutineVariadicMode,
    },
    SQLError,
};
use std::sync::Arc;
use uqa_core::{
    memory::{Produced, ProductionControl},
    RelationIdentity,
};

pub(super) struct RoutineMatch {
    pub function: Arc<SQLUserFunction>,
    pub signature: Produced<MatchedRoutineSignature>,
    pub invocation: Produced<RoutineInvocationBinding>,
}

impl RankedFunctionMatch for RoutineMatch {
    fn argument_types(&self) -> &[String] {
        &self.signature.argument_signature
    }
    fn raw_exact_matches(&self) -> usize {
        self.signature.raw_exact_matches
    }
    fn exact_matches(&self) -> usize {
        self.signature.exact_matches
    }
    fn preferred_matches(&self) -> usize {
        self.signature.preferred_matches
    }
    fn is_variadic_expansion(&self) -> bool {
        self.signature.variadic_mode == RoutineVariadicMode::Pack
    }
}

#[expect(
    clippy::result_large_err,
    reason = "the shared matcher retains diagnostic fields inline so reporting an admission failure needs no diagnostic allocation"
)]
pub(super) fn match_candidate(
    catalog: &RoutineTypeSnapshot,
    function: Arc<SQLUserFunction>,
    call: RoutineCallDescriptor<'_>,
    kind: RoutineCallKind,
    control: &ProductionControl<'_>,
) -> Result<Option<RoutineMatch>, RoutineSignatureProductionError> {
    let indices = parameters::indices(&function.def, kind, control)?;
    let parameters = parameters::descriptors(catalog, &function.def, &indices, control)?;
    let Some(signature) = match_routine_candidate_with_control(&parameters, call, control)? else {
        return Ok(None);
    };
    let invocation = invocation::binding(&function.def, &indices, &signature, control)?;
    Ok(Some(RoutineMatch {
        function,
        signature,
        invocation,
    }))
}

/// The first effective signature selects its visible schema; equal signatures in that schema stay eligible for ambiguity resolution.
pub(super) fn retain_earliest_effective_signatures(
    candidates: &mut Vec<RoutineMatch>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let mut index = 0;
    while index < candidates.len() {
        control.check()?;
        let candidate = &candidates[index];
        let schema = RelationIdentity::borrowed_reference(&candidate.function.def.name)
            .and_then(|reference| reference.schema);
        let mut shadowed = false;
        for previous in &candidates[..index] {
            control.check()?;
            if previous.argument_types() == candidate.argument_types() {
                let first_schema =
                    RelationIdentity::borrowed_reference(&previous.function.def.name)
                        .and_then(|reference| reference.schema);
                shadowed = first_schema != schema;
                break;
            }
        }
        if shadowed {
            candidates.remove(index);
        } else {
            index += 1;
        }
    }
    Ok(())
}
