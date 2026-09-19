//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture only physical primary keys touched by native triggers and cascades. Encoding functions perform no SQL, application callbacks or I/O.

use std::sync::Arc;

use parking_lot::Mutex;
use rusqlite::{
    functions::FunctionFlags,
    types::{ToSqlOutput, ValueRef},
    Connection, ToSql,
};
use uqa_core::memory::{BudgetedVec, MemoryReservation};
use uqa_storage::{mvcc::VersionError, read_control::StorageReadControl};

use super::{encode_row, invalid, NativeRecordFamily as Family};
use crate::mvcc::{Error, PhysicalResult};

struct EncodedKey {
    bytes: BudgetedVec<u8>,
    _result: MemoryReservation,
}

impl ToSql for EncodedKey {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::Borrowed(ValueRef::Blob(&self.bytes)))
    }
}

pub(super) struct Capture(Arc<Mutex<Option<VersionError>>>);

impl Capture {
    pub(super) fn install(
        connection: &Connection,
        control: &StorageReadControl,
    ) -> PhysicalResult<Self> {
        let errors = Arc::new(Mutex::new(None));
        let reported = Arc::clone(&errors);
        let control = control.clone();
        connection.create_scalar_function(
            "__uqa_mvcc_native_key",
            -1,
            FunctionFlags::SQLITE_UTF8
                | FunctionFlags::SQLITE_INNOCUOUS
                | FunctionFlags::SQLITE_DETERMINISTIC,
            move |context| {
                let encode = || -> Result<EncodedKey, VersionError> {
                    control.cancellation().check()?;
                    if context.is_empty() {
                        return Err(invalid("captured native key lacks its family"));
                    }
                    let family = context
                        .get::<u16>(0)
                        .ok()
                        .and_then(Family::from_id)
                        .ok_or_else(|| invalid("unknown captured native family"))?;
                    let layout = family.layout();
                    if context.len() != layout.primary_key.len() + 1 {
                        return Err(invalid("captured native primary key has the wrong width"));
                    }
                    let mut values = BudgetedVec::new(control.memory());
                    for (argument, &column) in layout.primary_key.iter().enumerate() {
                        let value = context.get_raw(argument + 1);
                        if !layout.column_types[column].accepts(value) {
                            return Err(invalid(
                                "captured native key has an invalid storage class",
                            ));
                        }
                        values.push(value)?;
                    }
                    let bytes = encode_row(&values, &control)?;
                    let result = control.memory().reserve(bytes.len())?;
                    Ok(EncodedKey {
                        bytes,
                        _result: result,
                    })
                };
                encode().map_err(|error| {
                    *reported.lock() = Some(error);
                    rusqlite::Error::UserFunctionError(Box::new(std::io::Error::other(
                        "native key capture failed",
                    )))
                })
            },
        )?;
        Ok(Self(errors))
    }

    pub(super) fn resolve<T>(&self, result: PhysicalResult<T>) -> PhysicalResult<T> {
        match (result, self.0.lock().take()) {
            (Err(_), Some(error)) => Err(Error::Version(error)),
            (result, _) => result,
        }
    }
}

pub(super) fn trigger(family: Family, action: &str) -> (String, String) {
    let layout = family.layout();
    let name = format!("_uqa_mvcc_native_capture_{}_{action}", family.id());
    let images: &[&str] = match action {
        "INSERT" => &["NEW"],
        "DELETE" => &["OLD"],
        _ => &["OLD", "NEW"],
    };
    let body = images.iter().map(|image| {
        let arguments = layout.primary_key.iter().map(|&column| format!("{image}.\"{}\"", layout.columns[column])).collect::<Vec<_>>().join(", ");
        format!("INSERT INTO _uqa_mvcc_native_changes(family, physical_key) VALUES ({}, __uqa_mvcc_native_key({}, {arguments})) ON CONFLICT(family, physical_key) DO NOTHING;", family.id(), family.id())
    }).collect::<Vec<_>>().join(" ");
    (
        name.clone(),
        format!(
            "CREATE TRIGGER {name} AFTER {action} ON {} BEGIN {body} END",
            layout.table
        ),
    )
}
