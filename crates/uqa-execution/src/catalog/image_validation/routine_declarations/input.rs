//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow the invoking type service only at a selected VARIADIC argument. Registry iterations and SQL continuations retain their own immutable inputs.

use crate::catalog::CatalogReadView;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{Budgeted, MemoryBudget, Produced, ProductionControl, RetainedValue},
    CancellationToken,
};
use uqa_sql::{
    catalog::named_definitions::NamedDefinitionIter,
    routines::{
        declaration::{
            catalog_variadic_array, CapturedDeclarationCheck, CapturedDeclarationStep,
            CapturedVariadicArgument, RoutineTypeCatalog,
        },
        registry::RoutineRegistry,
        SQLRoutine,
    },
    SQLError,
};

/// One lookup at the original argument position. A native session may return a retained catalog projection; a borrowed external type service is called only during the invoking advance.
pub trait DeclarationTypeLookup {
    fn variadic_lookup(
        &self,
        catalog: &CatalogReadView,
        canonical: Produced<String>,
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<VariadicLookup, SQLError>;
}

pub enum VariadicLookup {
    Resolved(bool),
    Reading(ImageDecoder<bool, SQLError>),
}

pub(in crate::catalog::image_validation) struct BorrowedTypes<'a>(pub &'a dyn RoutineTypeCatalog);

impl DeclarationTypeLookup for BorrowedTypes<'_> {
    fn variadic_lookup(
        &self,
        _: &CatalogReadView,
        canonical: Produced<String>,
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<VariadicLookup, SQLError> {
        let control = ProductionControl::new(memory, cancellation, cancellation);
        catalog_variadic_array(self.0, &canonical, &control).map(VariadicLookup::Resolved)
    }
}

type Registry = RetainedValue<RoutineRegistry>;
struct Overloads<'a> {
    groups: NamedDefinitionIter<'a, Vec<SQLRoutine>>,
    current: std::slice::Iter<'a, SQLRoutine>,
}
self_cell::self_cell!(
    struct Routines {
        owner: Registry,
        #[not_covariant]
        dependent: Overloads,
    }
);

enum Read {
    Group,
    Declaration(SQLRoutine, ImageDecoder<CapturedDeclarationStep, SQLError>),
    Lookup(
        SQLRoutine,
        CapturedVariadicArgument,
        ImageDecoder<bool, SQLError>,
    ),
}

pub(in crate::catalog::image_validation) struct Cursor {
    routines: Routines,
    read: Read,
}

impl Cursor {
    pub(in crate::catalog::image_validation) fn new(
        catalog: &CatalogReadView,
        memory: &MemoryBudget,
    ) -> Result<Budgeted<Box<Self>>, SQLError> {
        // The self-referential cell holds one immutable registry handle and its iterator in one allocation.
        let mut lease = memory.reserve(size_of::<Self>())?;
        lease.absorb(memory.reserve(size_of::<(Registry, Overloads<'static>)>())?);
        let routines = Routines::new(
            catalog.snapshot().definitions.sql_user_functions.clone(),
            |registry| Overloads {
                groups: registry.iter(),
                current: [].iter(),
            },
        );
        Ok(Budgeted::new(
            Box::new(Self {
                routines,
                read: Read::Group,
            }),
            lease,
        ))
    }

    pub(in crate::catalog::image_validation) fn advance(
        &mut self,
        catalog: &CatalogReadView,
        lookup: &dyn DeclarationTypeLookup,
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
        steps: usize,
    ) -> Result<bool, SQLError> {
        for _ in 0..steps {
            if self.step(catalog, lookup, memory, cancellation)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn step(
        &mut self,
        catalog: &CatalogReadView,
        lookup: &dyn DeclarationTypeLookup,
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<bool, SQLError> {
        let window = ImageDecodeWindow { bytes: 1, steps: 1 };
        cancellation.check()?;
        self.read = match std::mem::replace(&mut self.read, Read::Group) {
            Read::Group => {
                let next = self.routines.with_dependent_mut(|_, cursor| {
                    if let Some(routine) = cursor.current.next() {
                        Some(Some(routine.clone()))
                    } else {
                        cursor.groups.next().map(|(_, group)| {
                            cursor.current = group.iter();
                            None
                        })
                    }
                });
                match next {
                    None => return Ok(true),
                    Some(None) => Read::Group,
                    Some(Some(routine)) => declaration(
                        routine,
                        CapturedDeclarationCheck::default(),
                        memory,
                        cancellation,
                    )?,
                }
            }
            Read::Declaration(routine, mut input) => {
                match input.advance(&mut std::io::empty(), window)? {
                    None => Read::Declaration(routine, input),
                    Some(CapturedDeclarationStep::Complete) => Read::Group,
                    Some(CapturedDeclarationStep::Variadic(name, check)) => {
                        match lookup.variadic_lookup(catalog, name, memory, cancellation)? {
                            VariadicLookup::Resolved(array) => {
                                declaration(routine, check.resolve(array)?, memory, cancellation)?
                            }
                            VariadicLookup::Reading(lookup) => Read::Lookup(routine, check, lookup),
                        }
                    }
                }
            }
            Read::Lookup(routine, check, mut input) => {
                match input.advance(&mut std::io::empty(), window)? {
                    None => Read::Lookup(routine, check, input),
                    Some(array) => {
                        declaration(routine, check.resolve(array)?, memory, cancellation)?
                    }
                }
            }
        };
        Ok(false)
    }
}

fn declaration(
    routine: SQLRoutine,
    check: CapturedDeclarationCheck,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Read, SQLError> {
    let original = cancellation.clone();
    let source = routine.clone();
    let decoder = ImageDecoder::new(memory, cancellation, |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(input.memory, &original, input.cancellation);
        check
            .read_from_input(&source.def, &control, &mut input)
            .await
    })?;
    Ok(Read::Declaration(routine, decoder))
}
