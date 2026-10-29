//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Visibility retains admitted signature keys while consumers borrow catalog definitions.

use super::{RoutineRegistry, SQLUserFunction};
use crate::{
    ast::{FunctionBinding, FunctionVolatility},
    SQLError,
};
use std::{cmp::Ordering, sync::Arc};
use uqa_core::memory::{BudgetedMap, OwnedMap, Produced, ProductionControl, ProductionVec};

struct Signature {
    types: Produced<Vec<String>>,
    procedure: bool,
}
impl PartialEq for Signature {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Signature {}
impl PartialOrd for Signature {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Signature {
    fn cmp(&self, other: &Self) -> Ordering {
        self.types
            .as_slice()
            .cmp(other.types.as_slice())
            .then(self.procedure.cmp(&other.procedure))
    }
}

enum Seen {
    Ordinary(OwnedMap<Signature, ()>),
    Controlled(BudgetedMap<Signature, ()>),
}

impl Seen {
    fn new(control: &ProductionControl<'_>) -> Self {
        control.budget().map_or_else(
            || Self::Ordinary(OwnedMap::new()),
            |budget| Self::Controlled(BudgetedMap::new(budget)),
        )
    }
    fn insert(
        &mut self,
        function: &SQLUserFunction,
        control: &ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        let mut types = ProductionVec::new(*control);
        for parameter in function.def.identity_params() {
            types.push_produced(
                crate::type_resolution::canonical_routine_type_name_with_control(
                    &parameter.type_name,
                    control,
                )?,
            )?;
        }
        let signature = Signature {
            types: types.finish()?,
            procedure: function.def.is_procedure,
        };
        Ok(match self {
            Self::Ordinary(seen) => seen.insert(signature, ()).is_none(),
            Self::Controlled(seen) => seen.insert(signature, ())?.is_none(),
        })
    }
}

pub(super) fn visit_visible<'a, K: AsRef<str>>(
    registry: &'a RoutineRegistry,
    keys: impl IntoIterator<Item = K>,
    control: &ProductionControl<'_>,
    mut visit: impl FnMut(&'a Arc<SQLUserFunction>) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    control.check()?;
    let mut seen = Seen::new(control);
    for key in keys {
        control.check()?;
        if let Some(overloads) = registry.get(key.as_ref()) {
            for function in overloads {
                control.check()?;
                if seen.insert(function, control)? {
                    visit(function)?;
                }
            }
        }
    }
    Ok(())
}

pub fn routine_volatility_by_keys_with_control<'a>(
    registry: &RoutineRegistry,
    keys: impl IntoIterator<Item = &'a str>,
    control: &ProductionControl<'_>,
) -> Result<Option<FunctionVolatility>, SQLError> {
    let mut result = None;
    visit_visible(registry, keys, control, |function| {
        merge(&mut result, function.def.volatility);
        Ok(())
    })?;
    Ok(result)
}

pub fn bound_routine_volatility_with_control(
    registry: &RoutineRegistry,
    binding: &FunctionBinding,
    control: &ProductionControl<'_>,
) -> Result<Option<FunctionVolatility>, SQLError> {
    control.check()?;
    if binding.builtin {
        return Ok(None);
    }
    let Some(identity) = binding.object_id else {
        return routine_volatility_by_keys_with_control(
            registry,
            std::iter::once(binding.name.as_str()),
            control,
        );
    };
    let mut result = None;
    for function in registry.values().flatten() {
        control.check()?;
        if function.def.object_id == Some(identity) {
            merge(&mut result, function.def.volatility);
        }
    }
    Ok(result)
}

fn merge(result: &mut Option<FunctionVolatility>, volatility: FunctionVolatility) {
    use FunctionVolatility::{Immutable, Stable, Volatile};
    *result = Some(match (*result, volatility) {
        (Some(Volatile), _) | (_, Volatile) => Volatile,
        (Some(Stable), _) | (_, Stable) => Stable,
        _ => Immutable,
    });
}

#[cfg(test)]
mod tests;
