//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered native relationship checks against one immutable catalog view.

use super::routine_declarations::{
    BorrowedTypes, Cursor as DeclarationCursor, DeclarationTypeLookup,
};
use super::{
    validate_authority, validate_constraints, validate_hierarchies, validate_indexes,
    validate_name_uniqueness, validate_namespaces, validate_plan_relations,
    validate_routine_declarations, validate_routines, validate_sequence_functions,
    validate_sequences, validate_types,
};
use crate::catalog::{foreign::relationships as foreign, CatalogReadSnapshot, CatalogReadView};
use uqa_core::{
    image_io::{decode_image, ImageDecodeWindow, ImageDecoder},
    memory::{Budgeted, MemoryBudget, ProductionControl},
    CancellationToken,
};
use uqa_sql::{
    catalog::CatalogValidation,
    routines::{declaration::RoutineTypeCatalog, RoutineResolution},
    SQLError,
};

mod bindings;
use bindings::Binding;

type CatalogReader = Budgeted<Box<ImageDecoder<(), SQLError>>>;
type OpenBinding<'a> =
    dyn Fn(Binding, &RelationshipValidation) -> Result<CatalogValidation, SQLError> + 'a;

struct Inputs<'a> {
    view: &'a CatalogReadView,
    routines: &'a dyn RoutineResolution,
    types: &'a dyn RoutineTypeCatalog,
    memory: &'a MemoryBudget,
    cancellation: &'a CancellationToken,
}

#[derive(Clone, Copy)]
enum Predicate {
    Binding(Binding),
    Scheduled(Scheduled),
    Declarations,
}

#[derive(Clone, Copy)]
enum Scheduled {
    Namespaces,
    Names,
    Authority,
    Foreign,
    Types,
    PlanRelations,
    Hierarchies,
    Sequences,
    Indexes,
    Constraints,
    Routines,
    SequenceFunctions,
}

const ORDERED: &[Predicate] = &[
    Predicate::Scheduled(Scheduled::Namespaces),
    Predicate::Scheduled(Scheduled::Names),
    Predicate::Scheduled(Scheduled::Authority),
    Predicate::Scheduled(Scheduled::Foreign),
    Predicate::Scheduled(Scheduled::Types),
    Predicate::Scheduled(Scheduled::PlanRelations),
    Predicate::Binding(Binding::Expressions),
    Predicate::Declarations,
    Predicate::Scheduled(Scheduled::Hierarchies),
    Predicate::Scheduled(Scheduled::Sequences),
    Predicate::Scheduled(Scheduled::Indexes),
    Predicate::Scheduled(Scheduled::Constraints),
    Predicate::Binding(Binding::Constraints),
    Predicate::Binding(Binding::Indexes),
    Predicate::Binding(Binding::Partitions),
    Predicate::Scheduled(Scheduled::Routines),
    Predicate::Binding(Binding::Routines),
    Predicate::Binding(Binding::Events),
    Predicate::Scheduled(Scheduled::SequenceFunctions),
];

/// Ordered predicates of one immutable unpublished catalog. Retained binding validation keeps the original lookup session until the predicate completes.
pub struct RelationshipValidation {
    catalog: CatalogReadView,
    memory: MemoryBudget,
    cancellation: CancellationToken,
    remaining: &'static [Predicate],
    reading: Option<Reading>,
}

enum Reading {
    Catalog(CatalogReader),
    Binding(CatalogValidation),
    Declarations(Budgeted<Box<DeclarationCursor>>),
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
        self.advance_with_declaration_types(routines, types, &BorrowedTypes(types), steps)
    }

    /// Drive native declaration lookups against this catalog while other borrowed services remain local to this call.
    pub fn advance_with_declaration_types(
        self,
        routines: &dyn RoutineResolution,
        types: &dyn RoutineTypeCatalog,
        declarations: &dyn DeclarationTypeLookup,
        steps: usize,
    ) -> Result<Option<Self>, SQLError> {
        self.next(routines, types, declarations, Some(steps), None)
    }

    /// Retain the private session serving captured routine and type lookups while a binding crosses work windows.
    pub fn advance_with_retained_services<S, R>(
        self,
        owner: &S,
        services: fn(&S) -> &R,
        declarations: &dyn DeclarationTypeLookup,
        steps: usize,
    ) -> Result<Option<Self>, SQLError>
    where
        S: Clone + Send + Sync + 'static,
        R: RoutineResolution + RoutineTypeCatalog + Send + Sync + 'static,
    {
        let begin = |binding: Binding, state: &Self| binding.begin(state, owner.clone(), services);
        self.next(
            services(owner),
            services(owner),
            declarations,
            Some(steps),
            Some(&begin),
        )
    }

    fn advance_catalog(
        &mut self,
        steps: usize,
        begin: impl FnOnce(&Self) -> Result<Reading, SQLError>,
    ) -> Result<bool, SQLError> {
        let reading = match self.reading.take() {
            Some(reading) => reading,
            None => begin(self)?,
        };
        self.reading = match reading {
            Reading::Catalog(mut input) => input
                .as_mut()
                .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })?
                .is_none()
                .then_some(Reading::Catalog(input)),
            Reading::Binding(input) => input
                .advance(
                    &ProductionControl::new(&self.memory, &self.cancellation, &self.cancellation),
                    steps,
                )?
                .map(Reading::Binding),
            Reading::Declarations(_) => unreachable!("catalog predicate owns its reader"),
        };
        Ok(self.reading.is_none())
    }

    fn next(
        mut self,
        routines: &dyn RoutineResolution,
        types: &dyn RoutineTypeCatalog,
        declarations: &dyn DeclarationTypeLookup,
        steps: Option<usize>,
        bindings: Option<&OpenBinding<'_>>,
    ) -> Result<Option<Self>, SQLError> {
        if steps == Some(0) {
            return Err(SQLError::Internal(
                "catalog validation requires a nonzero work window".into(),
            ));
        }
        self.cancellation.check()?;
        let (predicate, remaining) = self
            .remaining
            .split_first()
            .expect("active relationship predicate");
        match *predicate {
            Predicate::Binding(binding) => {
                if let Some(steps) = steps.filter(|_| self.reading.is_some() || bindings.is_some())
                {
                    if !self.advance_catalog(steps, |state| {
                        bindings.expect("new binding owns its lookup services")(binding, state)
                            .map(Reading::Binding)
                    })? {
                        return Ok(Some(self));
                    }
                } else {
                    binding.inline(&Inputs {
                        view: &self.catalog,
                        routines,
                        types,
                        memory: &self.memory,
                        cancellation: &self.cancellation,
                    })?;
                }
            }
            Predicate::Declarations
                if !self
                    .catalog
                    .snapshot()
                    .definitions
                    .sql_user_functions
                    .is_empty() =>
            {
                if let Some(steps) = steps {
                    let mut cursor = match self.reading.take() {
                        Some(Reading::Declarations(cursor)) => cursor,
                        None => DeclarationCursor::new(&self.catalog, &self.memory)?,
                        Some(Reading::Binding(_) | Reading::Catalog(_)) => {
                            unreachable!("declaration validation owns its cursor")
                        }
                    };
                    if !cursor.as_mut().advance(
                        &self.catalog,
                        declarations,
                        &self.memory,
                        &self.cancellation,
                        steps,
                    )? {
                        self.reading = Some(Reading::Declarations(cursor));
                        return Ok(Some(self));
                    }
                } else {
                    validate_routine_declarations(
                        self.catalog.snapshot(),
                        types,
                        &ProductionControl::new(
                            &self.memory,
                            &self.cancellation,
                            &self.cancellation,
                        ),
                    )?;
                }
            }
            Predicate::Scheduled(check) if check.applies(self.catalog.snapshot()) => {
                if let Some(steps) = steps {
                    if !self.advance_catalog(steps, |state| {
                        check
                            .begin(&state.catalog, &state.memory, &state.cancellation)
                            .map(Reading::Catalog)
                    })? {
                        return Ok(Some(self));
                    }
                } else {
                    check.inline(&self.catalog, &self.memory, &self.cancellation)?;
                }
            }
            Predicate::Scheduled(_) | Predicate::Declarations => (),
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
            Self::PlanRelations => {
                let definitions = &snapshot.definitions;
                !definitions.views.is_empty()
                    || !definitions.sql_user_functions.is_empty()
                    || !definitions.triggers.is_empty()
                    || !definitions.rules.is_empty()
            }
            Self::Hierarchies => !snapshot.tables.is_empty(),
            Self::Sequences => {
                !snapshot.tables.is_empty()
                    || !snapshot.definitions.foreign_tables.is_empty()
                    || !snapshot.definitions.sequences.is_empty()
            }
            Self::Indexes => {
                !snapshot.tables.is_empty() || !snapshot.definitions.catalog_indexes.is_empty()
            }
            Self::Constraints => {
                !snapshot.tables.is_empty()
                    || !snapshot.definitions.foreign_tables.is_empty()
                    || !snapshot.definitions.domains.is_empty()
                    || !snapshot.definitions.triggers.is_empty()
                    || !snapshot.definitions.catalog_indexes.is_empty()
            }
            Self::Routines | Self::SequenceFunctions => {
                let d = &snapshot.definitions;
                !snapshot.tables.is_empty()
                    || !d.foreign_tables.is_empty()
                    || !d.domains.is_empty()
                    || !d.views.is_empty()
                    || !d.sql_user_functions.is_empty()
                    || !d.triggers.is_empty()
                    || !d.rules.is_empty()
            }
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
        // Select the actual continuation before admitting its frame. A match inside the future would reserve the largest unrelated predicate on every step.
        macro_rules! reader {
            ($input:ident => $read:expr) => {
                ImageDecoder::new(memory, cancellation, |mut reader| async move {
                    let $input = &mut reader.input();
                    $read.await
                })
            };
        }
        let input = match self {
            Self::Namespaces => {
                reader!(input => super::namespaces::from_input(&catalog, &original, input))
            }
            Self::Names => {
                reader!(input => super::name_claims::from_input(&catalog, &original, input))
            }
            Self::Authority => {
                reader!(input => super::authority::validate(catalog.snapshot(), input.memory, &original, input))
            }
            Self::Foreign => {
                reader!(input => foreign::validate(catalog.snapshot(), &original, input))
            }
            Self::Types => {
                reader!(input => super::type_dependencies::from_input(catalog.snapshot(), &original, input))
            }
            Self::PlanRelations => {
                reader!(input => super::plan_dependencies::validate_with_work(
                    &catalog, &ProductionControl::new(input.memory, &original, input.cancellation), input
                ))
            }
            Self::Hierarchies => {
                reader!(input => super::validate_hierarchies_with_work(catalog.snapshot(), input.memory, &original, input))
            }
            Self::Sequences => {
                reader!(input => super::validate_sequences_with_work(catalog.snapshot(), input.memory, &original, input))
            }
            Self::Indexes => {
                reader!(input => super::validate_indexes_with_work(catalog.snapshot(), input.memory, &original, input))
            }
            Self::Constraints => {
                reader!(input => super::validate_constraints_with_work(catalog.snapshot(), input.memory, &original, input))
            }
            Self::Routines => {
                reader!(input => super::routine_dependencies::validate_with_work(
                    catalog.snapshot(), input.memory, &original, input
                ))
            }
            Self::SequenceFunctions => {
                reader!(input => super::sequence_functions::validate_with_work(
                    &catalog, &ProductionControl::new(input.memory, &original, input.cancellation), input
                ))
            }
        }?;
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
            Self::PlanRelations => validate_plan_relations(
                catalog,
                &ProductionControl::new(memory, cancellation, cancellation),
            ),
            Self::Hierarchies => validate_hierarchies(catalog.snapshot(), memory, cancellation),
            Self::Sequences => validate_sequences(catalog.snapshot(), memory, cancellation),
            Self::Indexes => validate_indexes(catalog.snapshot(), memory, cancellation),
            Self::Constraints => validate_constraints(catalog.snapshot(), memory, cancellation),
            Self::Routines => validate_routines(catalog.snapshot(), memory, cancellation),
            Self::SequenceFunctions => validate_sequence_functions(
                catalog,
                &ProductionControl::new(memory, cancellation, cancellation),
            ),
            Self::Foreign => decode_image(
                &mut std::io::empty(),
                memory,
                cancellation,
                async |mut input| {
                    foreign::validate(catalog.snapshot(), cancellation, &mut input).await
                },
            ),
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
        remaining = checks.next(routines, types, &BorrowedTypes(types), None, None)?;
    }
    Ok(())
}

#[cfg(test)]
mod authority_tests;

#[cfg(test)]
mod table_tests;
