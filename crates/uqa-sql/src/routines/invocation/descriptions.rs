//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Successful routine descriptions belong to one immutable binding and catalog scope.

use crate::{
    assignment::routines::{
        coerce_routine_value_from, RoutineValueContext, RoutineValueDescription,
    },
    ast::{ColumnType, FunctionBinding},
    routines::SQLUserFunction,
    schema::retention::CatalogRetentionError,
    SQLError,
};
use std::sync::{Arc, OnceLock, Weak};
use uqa_core::{
    memory::{MemoryReservation, Produced, ProductionControl, ProductionVec},
    Value, ValueRetentionError,
};

/// Identity of the native routine registry and type-resolution environment. Adapters must decline or replace it whenever a bound routine or type can resolve differently.
#[derive(Clone, Default)]
pub struct RoutineDescriptionScope(Arc<()>);

impl RoutineDescriptionScope {
    pub fn matches(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// One optional description for an immutable prepared call, never one entry per input row.
#[derive(Default)]
pub struct RoutineCallDescriptions {
    description: OnceLock<Box<RoutineCallDescription>>,
}

impl std::fmt::Debug for RoutineCallDescriptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RoutineCallDescriptions")
            .field("retained", &self.description.get().is_some())
            .finish()
    }
}

pub struct RoutineCallDescription {
    scope: RoutineDescriptionScope,
    binding: Produced<FunctionBinding>,
    function: Weak<SQLUserFunction>,
    sources: Produced<Vec<Option<ColumnType>>>,
    arguments: Produced<Vec<OnceLock<Produced<RoutineValueDescription>>>>,
    parameters: Produced<Vec<OnceLock<Produced<RoutineValueDescription>>>>,
    _memory: Option<MemoryReservation>,
}

#[derive(Clone, Copy)]
pub struct RoutineConversionEnvironment<'a> {
    pub values: &'a dyn RoutineValueContext,
    pub scope: &'a dyn Fn() -> Option<RoutineDescriptionScope>,
    pub control: ProductionControl<'a>,
}

impl RoutineCallDescriptions {
    /// The entire binding and current scope must agree before any saved metadata is read.
    pub fn get(
        &self,
        binding: &FunctionBinding,
        scope: &RoutineDescriptionScope,
    ) -> Option<&RoutineCallDescription> {
        self.description
            .get()
            .filter(|saved| *saved.binding == *binding && saved.scope.matches(scope))
            .map(Box::as_ref)
    }

    /// Retain only a successful bound selection. Optional allocation failure leaves the live path intact; cancellation and malformed metadata are still errors.
    pub fn remember(
        &self,
        binding: &FunctionBinding,
        function: &Arc<SQLUserFunction>,
        sources: &[Option<ColumnType>],
        scope: RoutineDescriptionScope,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        if self.description.get().is_some() || !eligible(binding, sources.len()) {
            return Ok(());
        }
        match RoutineCallDescription::new(binding, function, sources, scope, control) {
            Ok(saved) => {
                let _ = self.description.set(saved);
                Ok(())
            }
            Err(CatalogRetentionError::Memory(_)) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

fn eligible(binding: &FunctionBinding, count: usize) -> bool {
    !binding.builtin
        && binding.object_id.is_some()
        && binding.invocation.as_ref().is_some_and(|invocation| {
            invocation.argument_sources.len() == count
                && invocation.argument_positions.len() == count
                && invocation.argument_targets.len() == count
                && invocation
                    .argument_sources
                    .iter()
                    .all(|name| name.as_deref().is_none_or(fixed_name))
                && invocation
                    .argument_targets
                    .iter()
                    .all(|name| fixed_name(name))
                && invocation
                    .parameter_types
                    .iter()
                    .all(|name| fixed_name(name))
        })
}

fn fixed_name(name: &str) -> bool {
    crate::ast::UserTypeIdentity::parse(name).is_some()
        || crate::parse_regtype_name(name)
            .ok()
            .flatten()
            .is_some_and(|name| name.names.len() == 2)
}

fn conversion_slots(
    count: usize,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<OnceLock<Produced<RoutineValueDescription>>>>, ValueRetentionError> {
    let mut slots = ProductionVec::new(*control);
    slots.reserve(count)?;
    for _ in 0..count {
        slots.push_produced(control.finish(OnceLock::new(), control.empty_reservation())?)?;
    }
    slots.finish()
}

impl RoutineCallDescription {
    fn new(
        binding: &FunctionBinding,
        function: &Arc<SQLUserFunction>,
        sources: &[Option<ColumnType>],
        scope: RoutineDescriptionScope,
        control: &ProductionControl<'_>,
    ) -> Result<Box<Self>, CatalogRetentionError> {
        let memory = control.reserve(std::mem::size_of::<Self>())?;
        let binding = binding.clone_with_control(control)?;
        let invocation = binding.invocation.as_ref().expect("eligible bound call");
        let arguments = conversion_slots(invocation.argument_targets.len(), control)?;
        let parameters = conversion_slots(invocation.parameter_types.len(), control)?;
        let mut copied = ProductionVec::new(*control);
        copied.reserve(sources.len())?;
        for source in sources {
            let (source, lease) = match source {
                Some(source) => {
                    let (source, lease) = source.clone_with_control(control)?.into_parts();
                    (Some(source), lease)
                }
                None => (None, control.empty_reservation()),
            };
            copied.push_produced(control.finish(source, lease)?)?;
        }
        Ok(Box::new(Self {
            scope,
            binding,
            function: Arc::downgrade(function),
            sources: copied.finish()?,
            arguments,
            parameters,
            _memory: memory,
        }))
    }

    pub fn function(&self) -> Option<Arc<SQLUserFunction>> {
        self.function.upgrade()
    }

    pub fn sources(&self) -> &[Option<ColumnType>] {
        &self.sources
    }

    pub fn is_current(&self, current: Option<&RoutineDescriptionScope>) -> bool {
        current.is_some_and(|current| self.scope.matches(current))
    }

    /// Each source/target read follows preceding value callbacks, and successful conversions publish descriptions only if that scope is still current afterward.
    pub fn coerce_argument(
        &self,
        index: usize,
        value: &Value,
        source: Option<&ColumnType>,
        environment: RoutineConversionEnvironment<'_>,
    ) -> Result<Value, SQLError> {
        let invocation = self
            .binding
            .invocation
            .as_ref()
            .expect("retained bound call");
        self.coerce(
            &invocation.argument_targets[index],
            &self.arguments[index],
            value,
            source,
            environment,
        )
    }

    pub fn coerce_parameter(
        &self,
        index: usize,
        value: &Value,
        source: Option<&ColumnType>,
        environment: RoutineConversionEnvironment<'_>,
    ) -> Result<Value, SQLError> {
        let invocation = self
            .binding
            .invocation
            .as_ref()
            .expect("retained bound call");
        self.coerce(
            &invocation.parameter_types[index],
            &self.parameters[index],
            value,
            source,
            environment,
        )
    }

    fn coerce(
        &self,
        name: &str,
        slot: &OnceLock<Produced<RoutineValueDescription>>,
        value: &Value,
        source: Option<&ColumnType>,
        environment: RoutineConversionEnvironment<'_>,
    ) -> Result<Value, SQLError> {
        let RoutineConversionEnvironment {
            values: context,
            scope,
            control,
        } = environment;
        control.check()?;
        if !self.is_current(scope().as_ref()) {
            return coerce_routine_value_from(context, value, name, source);
        }
        if let Some(description) = slot.get() {
            return description.coerce(context, value, source);
        }
        let Some(description) = RoutineValueDescription::resolve(context, name) else {
            return crate::expr::cast_value(value, name);
        };
        let result = description.coerce(context, value, source)?;
        if self.is_current(scope().as_ref()) {
            match description.clone_with_control(&control) {
                Ok(description) => {
                    let _ = slot.set(description);
                }
                Err(ValueRetentionError::Memory(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(result)
    }
}
