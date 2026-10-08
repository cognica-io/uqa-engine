//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::path::Path;

use uqa_storage::{mvcc::DatabaseId, read_control::StorageReadControl};

use crate::{
    mvcc::leases::{LeaseNamespace, NativeLeaseAdmission, NativeLeaseFile},
    Result, SQLiteError,
};

pub(super) struct Admission {
    file: NativeLeaseFile,
    _held: NativeLeaseAdmission,
}

/// Existing owner slots carried zero before exact shared row coordination.
const COORDINATION_PROTOCOL: u64 = 2;

pub(super) struct Owner {
    _lease: Box<dyn Send + Sync>,
    file: NativeLeaseFile,
}

impl Owner {
    pub(super) fn admit_records(&self, control: &StorageReadControl) -> Result<Admission> {
        let held = self.file.admit(control)?;
        let mut compatible = true;
        self.file.visit(control, &mut |protocol| {
            compatible &= protocol == COORDINATION_PROTOCOL;
            Ok(())
        })?;
        if !compatible {
            return Err(SQLiteError::DatabaseCoordinationUpgradeBusy);
        }
        Ok(Admission {
            file: self.file.clone(),
            _held: held,
        })
    }
}

impl Admission {
    pub(super) fn acquire(
        path: &Path,
        exclusive: bool,
        control: &StorageReadControl,
    ) -> Result<Self> {
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push(".uqa-owners");
        let file = NativeLeaseFile::open(
            Path::new(&sidecar),
            LeaseNamespace {
                magic: *b"UQAOWN01",
                // Physical path ownership spans every transaction-history incarnation.
                database: DatabaseId::from_bytes([0; 16]),
                incarnation: None,
            },
        )?;
        let held = file.admit(control)?;
        if exclusive {
            let mut occupied = false;
            file.visit(control, &mut |_| {
                occupied = true;
                Ok(())
            })?;
            if occupied {
                return Err(SQLiteError::DatabaseRestoreBusy);
            }
        }
        Ok(Self { file, _held: held })
    }

    pub(super) fn retain(&self, control: &StorageReadControl) -> Result<Owner> {
        Ok(Owner {
            _lease: self.file.retain(COORDINATION_PROTOCOL, control)?,
            file: self.file.clone(),
        })
    }
}
