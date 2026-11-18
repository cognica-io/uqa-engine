//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered native relationship checks against one immutable catalog view.

use super::{
    validate_authority, validate_constraint_bindings, validate_constraints,
    validate_event_bindings, validate_hierarchies, validate_index_bindings, validate_indexes,
    validate_name_uniqueness, validate_namespaces, validate_partition_bindings,
    validate_plan_relations, validate_routine_bindings, validate_routine_declarations,
    validate_routines, validate_sequence_functions, validate_sequences, validate_types,
};
use crate::catalog::{foreign::relationships as foreign, CatalogReadSnapshot, CatalogReadView};
use uqa_core::{
    image_io::{decode_image, AsyncImageInput, ImageDecodeWindow, ImageDecoder},
    memory::{Budgeted, MemoryBudget, ProductionControl},
    CancellationToken,
};
use uqa_sql::{
    routines::{declaration::RoutineTypeCatalog, RoutineResolution},
    SQLError,
};

struct Inputs<'a> {
    view: &'a CatalogReadView,
    snapshot: &'a CatalogReadSnapshot,
    routines: &'a dyn RoutineResolution,
    types: &'a dyn RoutineTypeCatalog,
    memory: &'a MemoryBudget,
    cancellation: &'a CancellationToken,
    production: ProductionControl<'a>,
}

#[derive(Clone, Copy)]
enum Predicate {
    Direct(fn(&Inputs<'_>) -> Result<(), SQLError>),
    Scheduled(Scheduled),
}

#[derive(Clone, Copy)]
enum Scheduled {
    Namespaces,
    Names,
    Authority,
    Foreign,
    Types,
}

const ORDERED: &[Predicate] = &[
    Predicate::Scheduled(Scheduled::Namespaces),
    Predicate::Scheduled(Scheduled::Names),
    Predicate::Scheduled(Scheduled::Authority),
    Predicate::Scheduled(Scheduled::Foreign),
    Predicate::Scheduled(Scheduled::Types),
    Predicate::Direct(|input| validate_plan_relations(input.view, &input.production)),
    Predicate::Direct(|input| {
        super::validate_stored_expressions(
            input.view,
            input.routines,
            input.memory,
            input.cancellation,
        )
    }),
    Predicate::Direct(|input| {
        validate_routine_declarations(input.snapshot, input.types, &input.production)
    }),
    Predicate::Direct(|input| {
        validate_hierarchies(input.snapshot, input.memory, input.cancellation)
    }),
    Predicate::Direct(|input| validate_sequences(input.snapshot, input.memory, input.cancellation)),
    Predicate::Direct(|input| validate_indexes(input.snapshot, input.memory, input.cancellation)),
    Predicate::Direct(|input| {
        validate_constraints(input.snapshot, input.memory, input.cancellation)
    }),
    Predicate::Direct(|input| {
        validate_constraint_bindings(input.view, input.routines, input.memory, input.cancellation)
    }),
    Predicate::Direct(|input| {
        validate_index_bindings(input.view, input.routines, input.memory, input.cancellation)
    }),
    Predicate::Direct(|input| {
        validate_partition_bindings(input.view, input.routines, input.memory, input.cancellation)
    }),
    Predicate::Direct(|input| validate_routines(input.snapshot, input.memory, input.cancellation)),
    Predicate::Direct(|input| {
        validate_routine_bindings(
            input.view,
            input.routines,
            input.types,
            input.memory,
            input.cancellation,
        )
    }),
    Predicate::Direct(|input| {
        validate_event_bindings(input.view, input.routines, input.memory, input.cancellation)
    }),
    Predicate::Direct(|input| validate_sequence_functions(input.view, &input.production)),
];

/// The outstanding predicates of an unpublished catalog. Each advance borrows the private
/// session only for its own call and preserves the same first error as synchronous validation.
pub struct RelationshipValidation {
    catalog: CatalogReadView,
    memory: MemoryBudget,
    cancellation: CancellationToken,
    remaining: &'static [Predicate],
    reading: Option<Budgeted<Box<ImageDecoder<(), SQLError>>>>,
}

impl RelationshipValidation {
    pub fn new(
        catalog: CatalogReadView,
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Self {
        Self {
            catalog,
            memory: memory.clone(),
            cancellation: cancellation.clone(),
            remaining: ORDERED,
            reading: None,
        }
    }

    pub fn advance(
        self,
        routines: &dyn RoutineResolution,
        types: &dyn RoutineTypeCatalog,
    ) -> Result<Option<Self>, SQLError> {
        self.advance_with_steps(routines, types, 1)
    }

    pub fn advance_with_steps(
        self,
        routines: &dyn RoutineResolution,
        types: &dyn RoutineTypeCatalog,
        steps: usize,
    ) -> Result<Option<Self>, SQLError> {
        if steps == 0 {
            return Err(SQLError::Internal(
                "catalog validation requires a nonzero work window".into(),
            ));
        }
        self.next(routines, types, Some(steps))
    }

    fn next(
        mut self,
        routines: &dyn RoutineResolution,
        types: &dyn RoutineTypeCatalog,
        steps: Option<usize>,
    ) -> Result<Option<Self>, SQLError> {
        self.cancellation.check()?;
        let (predicate, remaining) = self
            .remaining
            .split_first()
            .expect("active relationship predicate");
        match *predicate {
            Predicate::Direct(predicate) => predicate(&Inputs {
                view: &self.catalog,
                snapshot: self.catalog.snapshot(),
                routines,
                types,
                memory: &self.memory,
                cancellation: &self.cancellation,
                production: ProductionControl::new(
                    &self.memory,
                    &self.cancellation,
                    &self.cancellation,
                ),
            })?,
            Predicate::Scheduled(check) if check.applies(self.catalog.snapshot()) => {
                if let Some(steps) = steps {
                    let mut reading = match self.reading.take() {
                        Some(reading) => reading,
                        None => check.begin(&self.catalog, &self.memory, &self.cancellation)?,
                    };
                    if reading
                        .as_mut()
                        .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })?
                        .is_none()
                    {
                        self.reading = Some(reading);
                        return Ok(Some(self));
                    }
                } else {
                    check.inline(&self.catalog, &self.memory, &self.cancellation)?;
                }
            }
            Predicate::Scheduled(_) => (),
        }
        self.cancellation.check()?;
        self.remaining = remaining;
        Ok((!remaining.is_empty()).then_some(self))
    }
}

impl Scheduled {
    fn applies(self, snapshot: &CatalogReadSnapshot) -> bool {
        match self {
            Self::Namespaces | Self::Names | Self::Authority | Self::Types => true,
            Self::Foreign => foreign::has_bindings(snapshot),
        }
    }

    fn begin(
        self,
        catalog: &CatalogReadView,
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<Budgeted<Box<ImageDecoder<(), SQLError>>>, SQLError> {
        let header = memory.reserve(size_of::<ImageDecoder<(), SQLError>>())?;
        let catalog = catalog.clone();
        let original = cancellation.clone();
        let input = ImageDecoder::new(memory, cancellation, |mut reader| async move {
            self.read(&catalog, &original, &mut reader.input()).await
        })?;
        Ok(Budgeted::new(Box::new(input), header))
    }

    fn inline(
        self,
        catalog: &CatalogReadView,
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<(), SQLError> {
        match self {
            Self::Namespaces => validate_namespaces(
                catalog,
                &ProductionControl::new(memory, cancellation, cancellation),
            ),
            Self::Names => validate_name_uniqueness(catalog.snapshot(), memory, cancellation),
            Self::Authority => validate_authority(catalog.snapshot(), memory, cancellation),
            Self::Types => validate_types(catalog.snapshot(), memory, cancellation),
            Self::Foreign => decode_image(
                &mut std::io::empty(),
                memory,
                cancellation,
                async |mut input| self.read(catalog, cancellation, &mut input).await,
            ),
        }
    }

    async fn read(
        self,
        catalog: &CatalogReadView,
        cancellation: &CancellationToken,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<(), SQLError> {
        match self {
            Self::Namespaces => super::namespaces::from_input(catalog, cancellation, input).await,
            Self::Names => super::name_claims::from_input(catalog, cancellation, input).await,
            Self::Authority => {
                super::authority::validate(catalog.snapshot(), input.memory, cancellation, input)
                    .await
            }
            Self::Foreign => foreign::validate(catalog.snapshot(), cancellation, input).await,
            Self::Types => {
                super::type_dependencies::from_input(catalog.snapshot(), cancellation, input).await
            }
        }
    }
}

pub fn validate(
    catalog: &CatalogReadView,
    routines: &dyn RoutineResolution,
    types: &dyn RoutineTypeCatalog,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    let mut remaining = Some(RelationshipValidation::new(
        catalog.clone(),
        memory,
        cancellation,
    ));
    while let Some(checks) = remaining {
        remaining = checks.next(routines, types, None)?;
    }
    Ok(())
}

#[cfg(test)]
mod authority_tests;
