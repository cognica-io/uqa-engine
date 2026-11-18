//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{sql_error, StoredFunctionSite};
use parking_lot::Mutex;
use uqa_core::image_io::AsyncImageInput;
use uqa_sql::{
    routines::{registry::CapturedRoutines, RoutineBody},
    SQLError,
};
use uqa_storage::{
    backup::BackupSourceStatistics, read_control::StorageReadControl, StorageBackendResult,
};

pub(super) use uqa_execution::routines::catalog::input::visit_stored_routine_runtime_calls as record;

pub(super) async fn native<'a, 'input: 'a, F>(
    routines: &'a CapturedRoutines,
    source: &'a StorageReadControl,
    input: &'a mut AsyncImageInput<'input>,
    counts: &'a Mutex<BackupSourceStatistics>,
    visit: &'a mut F,
) -> StorageBackendResult<()>
where
    F: FnMut(&str, StoredFunctionSite) -> Result<(), SQLError> + Send,
{
    for (_, overloads) in routines {
        input.checkpoint().await?;
        for routine in overloads {
            input.checkpoint().await?;
            counts.lock().visited_positions += 1;
            let compiled = match &routine.body {
                RoutineBody::Bound(body) => Some(&***body),
                RoutineBody::Source => None,
            };
            uqa_sql::routines::native::visit_stored_routine_runtime_calls(
                &routine.def,
                compiled,
                source.cancellation(),
                input,
                visit,
            )
            .await
            .map_err(sql_error)?;
        }
    }
    Ok(())
}
