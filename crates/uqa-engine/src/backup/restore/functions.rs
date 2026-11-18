//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::RestoreBindings;
use uqa_core::memory::{Budgeted, ProductionControl};
use uqa_storage::{
    backup::{
        BackupFunctionKind as Kind, BackupFunctionReader, BackupLease, BackupResult,
        BackupSourceStep,
    },
    read_control::StorageReadControl,
    StorageBackendError,
};

pub(super) struct Admission<'a>(Budgeted<Box<BackupFunctionReader<'a>>>);

impl<'a> Admission<'a> {
    pub(super) fn open(
        lease: &BackupLease<'a>,
        control: &StorageReadControl,
    ) -> BackupResult<Option<Self>> {
        let Some(reader) = lease.function_references(control)? else {
            return Ok(None);
        };
        let memory = control
            .memory()
            .reserve(size_of::<BackupFunctionReader<'_>>())?;
        Ok(Some(Self(Budgeted::new(Box::new(reader), memory))))
    }

    pub(super) fn advance(
        admission: &mut Option<Self>,
        maximum_bytes: usize,
        bindings: &RestoreBindings,
        control: &StorageReadControl,
    ) -> BackupResult<bool> {
        let Some(work) = admission else {
            return Ok(true);
        };
        match work.0.as_mut().advance(maximum_bytes)? {
            BackupSourceStep::Pending => Ok(false),
            BackupSourceStep::Complete => {
                *admission = None;
                Ok(true)
            }
            BackupSourceStep::Unit => {
                let reference = work.0.current().expect("selected function declaration");
                if !crate::backup::functions::matches(
                    reference,
                    bindings.registered_kinds(reference.name),
                ) {
                    let production = ProductionControl::new(
                        control.memory(),
                        control.cancellation(),
                        control.cancellation(),
                    );
                    let requirement = match reference.kind {
                        Kind::Scalar => "a scalar registration",
                        Kind::Table => "a table registration",
                        Kind::Aggregate => "an aggregate registration",
                        Kind::AbsentScalar => "no scalar registration",
                        Kind::AbsentTable => "no table registration",
                        Kind::AbsentAggregate => "no aggregate registration",
                    };
                    let message = production
                        .format(format_args!(
                            "captured function {} requires {requirement}",
                            reference.name
                        ))
                        .map_err(StorageBackendError::from)?;
                    let error =
                        crate::SQLError::admit_routine_message("42883", message, &production)
                            .map_err(|error| {
                                StorageBackendError::backend(
                                    "application function diagnostic",
                                    error,
                                )
                            })?;
                    return Err(
                        StorageBackendError::CatalogBinding(std::sync::Arc::new(error)).into(),
                    );
                }
                Ok(false)
            }
        }
    }
}
