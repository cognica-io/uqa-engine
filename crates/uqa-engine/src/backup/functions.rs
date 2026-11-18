//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Select application lookups on the same committed catalog and registry cut as the image.

use crate::{
    sql_registry::{CapturedDeclarations, RegisteredKinds},
    Engine, SQLError,
};
use parking_lot::Mutex;
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{Budgeted, OwnedMap, OwnedMapIntoIter, Produced, ProductionControl, RetainedValue},
};
use uqa_sql::binding::view_dependencies::StoredFunctionSite;
use uqa_storage::{
    backup::{
        BackupFunctionKind as Kind, BackupFunctionReference, BackupFunctionSource, BackupResult,
        BackupSourceStatistics, BackupSourceStep,
    },
    read_control::StorageReadControl,
    StorageBackendError,
};

mod catalogs;
pub(super) use catalogs::Catalogs;

pub(super) struct References {
    stage: Stage,
    current: Option<(Produced<String>, u8)>,
    kind: Option<Kind>,
    statistics: RetainedValue<Mutex<BackupSourceStatistics>>,
}

type SortedNames = Budgeted<Box<OwnedMapIntoIter<String, u8>>>;

enum Stage {
    Reading(ImageDecoder<Option<SortedNames>, StorageBackendError>),
    Output(Option<SortedNames>),
}

impl References {
    pub(super) fn new(
        catalogs: Catalogs,
        declarations: CapturedDeclarations,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        let statistics = RetainedValue::with_budget(
            Mutex::new(BackupSourceStatistics::default()),
            control.memory(),
        )?;
        let counts = statistics.clone();
        let original = control.clone();
        let input = ImageDecoder::new(
            control.memory(),
            control.cancellation(),
            move |mut reader| async move {
                let mut input = reader.input();
                let production = ProductionControl::new(
                    original.memory(),
                    original.cancellation(),
                    input.cancellation,
                );
                let mut selected = OwnedMap::new();
                let mut visit = |name: &str, site| {
                    let name = uqa_sql::semantics::lowercase_function_name_with_control(
                        name.trim(),
                        &production,
                    )?;
                    let kinds = declarations.get(name.as_str()).copied().unwrap_or_default();
                    let required = selection(site, &name, kinds, &production)?;
                    if let Some(current) = selected.get_mut(name.as_str()) {
                        *current |= required;
                    } else if required != 0 {
                        let name = production
                            .copy_text(&name)?
                            .into_budgeted()
                            .unwrap_or_else(|_| unreachable!("admitted backup names"));
                        selected.insert_admitted_key(name, required)?;
                    }
                    Ok(())
                };
                catalogs
                    .visit(&original, &mut input, &counts, &mut visit)
                    .await?;
                if selected.is_empty() {
                    return Ok(None);
                }
                let memory = original
                    .memory()
                    .reserve(size_of::<OwnedMapIntoIter<String, u8>>())?;
                Ok(Some(Budgeted::new(Box::new(selected.into_iter()), memory)))
            },
        )
        .map_err(StorageBackendError::from)?;
        Ok(Self {
            stage: Stage::Reading(input),
            current: None,
            kind: None,
            statistics,
        })
    }

    pub(super) fn staged(engine: &Engine, control: &StorageReadControl) -> BackupResult<Self> {
        let production = ProductionControl::new(
            control.memory(),
            control.cancellation(),
            control.cancellation(),
        );
        let catalogs = Catalogs::staged(engine, &production).map_err(sql_error)?;
        let registrations = engine
            .extensions
            .functions
            .capture_declarations(control.memory())
            .map_err(|error| sql_error(error.into()))?;
        Self::new(catalogs, registrations, control)
    }
}

const KINDS: [Kind; 6] = [
    Kind::Scalar,
    Kind::Table,
    Kind::Aggregate,
    Kind::AbsentScalar,
    Kind::AbsentTable,
    Kind::AbsentAggregate,
];

fn bit(kind: Kind) -> u8 {
    1 << (kind as u8 - 1)
}

fn selection(
    site: StoredFunctionSite,
    name: &str,
    kinds: RegisteredKinds,
    control: &ProductionControl<'_>,
) -> Result<u8, SQLError> {
    use uqa_sql::binding::view_dependencies::ApplicationFunctionKind;
    let mut required = 0;
    site.visit_application_lookups(name, control, |kind| {
        let present = kinds.contains(kind);
        let selected = match (kind, present) {
            (ApplicationFunctionKind::Scalar, true) => Kind::Scalar,
            (ApplicationFunctionKind::Table, true) => Kind::Table,
            (ApplicationFunctionKind::Aggregate, true) => Kind::Aggregate,
            (ApplicationFunctionKind::Scalar, false) => Kind::AbsentScalar,
            (ApplicationFunctionKind::Table, false) => Kind::AbsentTable,
            (ApplicationFunctionKind::Aggregate, false) => Kind::AbsentAggregate,
        };
        required |= bit(selected);
        present
    })?;
    Ok(required)
}

pub(super) fn matches(reference: BackupFunctionReference<'_>, kinds: RegisteredKinds) -> bool {
    match reference.kind {
        Kind::Scalar => kinds.scalar,
        Kind::Table => kinds.table,
        Kind::Aggregate => kinds.aggregate,
        Kind::AbsentScalar => !kinds.scalar,
        Kind::AbsentTable => !kinds.table,
        Kind::AbsentAggregate => !kinds.aggregate,
    }
}

fn sql_error(error: SQLError) -> StorageBackendError {
    match error {
        SQLError::Cancelled(reason) => reason.into(),
        error => StorageBackendError::CatalogBinding(Arc::new(error)),
    }
}

impl BackupFunctionSource for References {
    fn advance(&mut self, maximum_positions: usize) -> BackupResult<BackupSourceStep> {
        if let Stage::Reading(input) = &mut self.stage {
            let Some(names) = input.advance(
                &mut std::io::empty(),
                ImageDecodeWindow {
                    bytes: 1,
                    steps: maximum_positions,
                },
            )?
            else {
                return Ok(BackupSourceStep::Pending);
            };
            self.stage = Stage::Output(names);
        }
        let Stage::Output(names) = &mut self.stage else {
            unreachable!()
        };
        if self.current.as_ref().is_none_or(|(_, mask)| *mask == 0) {
            self.current = names
                .as_mut()
                .and_then(|names| names.as_mut().next_retained());
        }
        self.kind = None;
        let Some((_, mask)) = &mut self.current else {
            return Ok(BackupSourceStep::Complete);
        };
        let kind = KINDS[mask.trailing_zeros() as usize];
        *mask &= !bit(kind);
        self.kind = Some(kind);
        Ok(BackupSourceStep::Unit)
    }

    fn current(&self) -> Option<BackupFunctionReference<'_>> {
        let (name, _) = self.current.as_ref()?;
        Some(BackupFunctionReference {
            name,
            kind: self.kind?,
        })
    }

    fn statistics(&self) -> BackupSourceStatistics {
        *self.statistics.lock()
    }
}
