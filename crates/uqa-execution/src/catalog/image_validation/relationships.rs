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
    validate_authority, validate_constraint_bindings, validate_constraints,
    validate_event_bindings, validate_hierarchies, validate_index_bindings, validate_indexes,
    validate_name_uniqueness, validate_namespaces, validate_partition_bindings,
    validate_plan_relations, validate_routine_bindings, validate_routine_declarations,
    validate_routines, validate_sequence_functions, validate_sequences, validate_types,
};
use crate::catalog::{foreign::relationships as foreign, CatalogReadSnapshot, CatalogReadView};
use uqa_core::{
    image_io::{decode_image, ImageDecodeWindow, ImageDecoder},
    memory::{Budgeted, MemoryBudget, ProductionControl},
    CancellationToken,
};
use uqa_sql::{
    routines::{declaration::RoutineTypeCatalog, RoutineResolution},
    SQLError,
};

struct Inputs<'a> {
    view: &'a CatalogReadView,
    routines: &'a dyn RoutineResolution,
    types: &'a dyn RoutineTypeCatalog,
    memory: &'a MemoryBudget,
    cancellation: &'a CancellationToken,
}

#[derive(Clone, Copy)]
enum Predicate {
    Direct(fn(&Inputs<'_>) -> Result<(), SQLError>),
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
    Predicate::Direct(|input| {
        super::validate_stored_expressions(
            input.view,
            input.routines,
            input.memory,
            input.cancellation,
        )
    }),
    Predicate::Declarations,
    Predicate::Scheduled(Scheduled::Hierarchies),
    Predicate::Scheduled(Scheduled::Sequences),
    Predicate::Scheduled(Scheduled::Indexes),
    Predicate::Scheduled(Scheduled::Constraints),
    Predicate::Direct(|input| {
        validate_constraint_bindings(input.view, input.routines, input.memory, input.cancellation)
    }),
    Predicate::Direct(|input| {
        validate_index_bindings(input.view, input.routines, input.memory, input.cancellation)
    }),
    Predicate::Direct(|input| {
        validate_partition_bindings(input.view, input.routines, input.memory, input.cancellation)
    }),
    Predicate::Scheduled(Scheduled::Routines),
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
    Predicate::Scheduled(Scheduled::SequenceFunctions),
];

/// The outstanding predicates of an unpublished catalog. Each advance borrows the private
/// session only for its own call and preserves the same first error as synchronous validation.
pub struct RelationshipValidation {
    catalog: CatalogReadView,
    memory: MemoryBudget,
    cancellation: CancellationToken,
    remaining: &'static [Predicate],
    reading: Option<Reading>,
}

enum Reading {
    Catalog(Budgeted<Box<ImageDecoder<(), SQLError>>>),
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
        if steps == 0 {
            return Err(SQLError::Internal(
                "catalog validation requires a nonzero work window".into(),
            ));
        }
        self.next(routines, types, declarations, Some(steps))
    }

    fn next(
        mut self,
        routines: &dyn RoutineResolution,
        types: &dyn RoutineTypeCatalog,
        declarations: &dyn DeclarationTypeLookup,
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
                routines,
                types,
                memory: &self.memory,
                cancellation: &self.cancellation,
            })?,
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
                        Some(Reading::Catalog(_)) => {
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
                    let mut reading = match self.reading.take() {
                        Some(Reading::Catalog(reading)) => reading,
                        Some(Reading::Declarations(_)) => {
                            unreachable!("catalog predicate owns its reader")
                        }
                        None => check.begin(&self.catalog, &self.memory, &self.cancellation)?,
                    };
                    if reading
                        .as_mut()
                        .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })?
                        .is_none()
                    {
                        self.reading = Some(Reading::Catalog(reading));
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
        remaining = checks.next(routines, types, &BorrowedTypes(types), None)?;
    }
    Ok(())
}

#[cfg(test)]
mod authority_tests;

#[cfg(test)]
mod table_tests;
