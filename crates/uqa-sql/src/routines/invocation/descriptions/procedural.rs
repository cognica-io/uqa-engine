//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Concrete procedural declarations retain metadata, never activation values.

use super::{
    conversion_slots, fixed_name, ColumnType, Produced, ProductionControl, RoutineCallDescription,
    RoutineConversionEnvironment, RoutineValueDescription, SQLError, Value, ValueRetentionError,
};
use crate::plpgsql::{PLpgSQLCompilationIdentity, PLpgSQLFunction};
use std::sync::OnceLock;
use uqa_core::memory::MemoryReservation;

pub(super) struct ProceduralSlots {
    compilation: PLpgSQLCompilationIdentity,
    datums: Produced<Vec<OnceLock<Produced<NamedDatum>>>>,
    result: OnceLock<Produced<NamedDatum>>,
    _memory: Option<MemoryReservation>,
}

struct NamedDatum {
    name: Produced<String>,
    ty: OnceLock<Produced<ColumnType>>,
    conversion: OnceLock<Produced<RoutineValueDescription>>,
}

/// A view of one retained compilation's slots. Every read still checks the call's
/// current scope and exact declared spelling; record fields do not use these slots.
#[derive(Clone, Copy)]
pub struct ProceduralRoutineDescription<'a> {
    call: &'a RoutineCallDescription,
    slots: &'a ProceduralSlots,
}

impl RoutineCallDescription {
    /// Admit at most one compilation's datum table for this bound call. Resolving
    /// declarations remains lazy at the interpreter's original read/conversion sites.
    pub fn procedural_body(
        &self,
        parsed: &PLpgSQLFunction,
        environment: RoutineConversionEnvironment<'_>,
    ) -> Result<Option<ProceduralRoutineDescription<'_>>, SQLError> {
        environment.control.check()?;
        if !self.is_current((environment.scope)().as_ref()) {
            return Ok(None);
        }
        if self.procedural.get().is_none() {
            match ProceduralSlots::new(parsed, &environment.control) {
                Ok(slots) => {
                    let _ = self.procedural.set(slots);
                }
                Err(ValueRetentionError::Memory(_)) => return Ok(None),
                Err(error) => return Err(error.into()),
            }
        }
        Ok(self
            .procedural
            .get()
            .filter(|slots| {
                slots.compilation.same(&parsed.compilation)
                    && slots.datums.len() == parsed.datums.len()
            })
            .map(|slots| ProceduralRoutineDescription { call: self, slots }))
    }
}

impl ProceduralSlots {
    fn new(
        parsed: &PLpgSQLFunction,
        control: &ProductionControl<'_>,
    ) -> Result<Box<Self>, ValueRetentionError> {
        let memory = control.reserve(size_of::<Self>())?;
        let datums = conversion_slots(parsed.datums.len(), control)?;
        Ok(Box::new(Self {
            compilation: parsed.compilation.clone(),
            datums,
            result: OnceLock::new(),
            _memory: memory,
        }))
    }
}

impl ProceduralRoutineDescription<'_> {
    fn named<'a>(
        &self,
        slot: Option<&'a OnceLock<Produced<NamedDatum>>>,
        name: &str,
        environment: RoutineConversionEnvironment<'_>,
    ) -> Result<Option<&'a NamedDatum>, SQLError> {
        environment.control.check()?;
        if !self.call.is_current((environment.scope)().as_ref()) {
            return Ok(None);
        }
        let Some(slot) = slot else { return Ok(None) };
        if let Some(saved) = slot.get() {
            return Ok((saved.name.as_str() == name).then_some(saved));
        }
        if !fixed_name(name) {
            return Ok(None);
        }
        match environment.control.copy_text(name) {
            Ok(copied_name) => {
                let saved = environment.control.finish(
                    NamedDatum {
                        name: copied_name,
                        ty: OnceLock::new(),
                        conversion: OnceLock::new(),
                    },
                    environment.control.empty_reservation(),
                )?;
                let _ = slot.set(saved);
                Ok(slot
                    .get()
                    .filter(|saved| saved.name.as_str() == name)
                    .map(|s| &**s))
            }
            Err(ValueRetentionError::Memory(_)) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Preserve the catalog-only declaration lookup and unresolved/pseudo-type
    /// fallback. Successful concrete metadata is reusable only in the same scope.
    pub fn datum_type(
        &self,
        index: usize,
        name: &str,
        environment: RoutineConversionEnvironment<'_>,
    ) -> Result<Option<ColumnType>, SQLError> {
        let saved = self.named(self.slots.datums.get(index), name, environment)?;
        if let Some(ty) = saved.and_then(|saved| saved.ty.get()) {
            return Ok(Some((**ty).clone()));
        }
        let ty = environment.values.catalog_column_type(name);
        if self.call.is_current((environment.scope)().as_ref()) {
            if let (Some(saved), Some(ty)) = (saved, &ty) {
                match ty.clone_with_control(&environment.control) {
                    Ok(copied) => {
                        let _ = saved.ty.set(copied);
                    }
                    Err(ValueRetentionError::Memory(_)) => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        Ok(ty)
    }

    /// Declaration initialization and assignment share the original per-value
    /// conversion kernel; each callback and domain predicate still executes.
    pub fn coerce_datum(
        &self,
        index: usize,
        name: &str,
        value: &Value,
        source: Option<&ColumnType>,
        environment: RoutineConversionEnvironment<'_>,
    ) -> Result<Value, SQLError> {
        self.coerce(
            self.slots.datums.get(index),
            name,
            value,
            source,
            environment,
        )
    }

    pub fn coerce_result(
        &self,
        name: &str,
        value: &Value,
        source: Option<&ColumnType>,
        environment: RoutineConversionEnvironment<'_>,
    ) -> Result<Value, SQLError> {
        self.coerce(Some(&self.slots.result), name, value, source, environment)
    }

    fn coerce(
        &self,
        slot: Option<&OnceLock<Produced<NamedDatum>>>,
        name: &str,
        value: &Value,
        source: Option<&ColumnType>,
        environment: RoutineConversionEnvironment<'_>,
    ) -> Result<Value, SQLError> {
        match self.named(slot, name, environment)? {
            Some(saved) => self
                .call
                .coerce(name, &saved.conversion, value, source, environment),
            None => crate::assignment::routines::coerce_routine_value_from(
                environment.values,
                value,
                name,
                source,
            ),
        }
    }
}
