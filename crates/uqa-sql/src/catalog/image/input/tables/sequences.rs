//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence declarations retain written metadata without creation or name binding.

use super::{invalid, payload, Input, MemoryReservation, SQLError};
use crate::ast::{
    AutoIncrement, AutoIncrementKind, AutoIncrementOwner, DeferredSQLError,
    IdentitySequenceDeclaration, IdentitySequenceName, RelationPersistence, SequenceDeclaration,
    SequenceOptionValue,
};

impl Input<'_> {
    pub(super) fn auto_increment(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<AutoIncrement, SQLError> {
        let kind = match self.binary.array::<1>()?[0] {
            0 => AutoIncrementKind::Serial,
            1 => AutoIncrementKind::IdentityAlways,
            2 => AutoIncrementKind::IdentityByDefault,
            3 => AutoIncrementKind::Legacy,
            _ => return Err(invalid("column sequence kind")),
        };
        let sequence = payload(self.optional_text()?, memory);
        let owner = if self.flag()? {
            Some(AutoIncrementOwner {
                table: payload(self.binary.text()?, memory),
                column: payload(self.binary.text()?, memory),
            })
        } else {
            None
        };
        let declaration = if self.flag()? {
            memory.grow(size_of::<IdentitySequenceDeclaration>())?;
            Some(Box::new(self.identity_declaration(memory)?))
        } else {
            None
        };
        Ok(AutoIncrement {
            kind,
            sequence,
            owner,
            declaration,
        })
    }

    fn identity_declaration(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<IdentitySequenceDeclaration, SQLError> {
        let name = if self.flag()? {
            Some(IdentitySequenceName {
                schema: payload(self.optional_text()?, memory),
                name: payload(self.binary.text()?, memory),
            })
        } else {
            None
        };
        let persistence = match self.binary.array::<1>()?[0] {
            0 => None,
            1 => Some(RelationPersistence::Permanent),
            2 => Some(RelationPersistence::Unlogged),
            3 => Some(RelationPersistence::Temporary),
            _ => return Err(invalid("identity sequence persistence")),
        };
        let has_error = self.flag()?;
        let sequence = self.sequence_declaration(memory)?;
        let error = if has_error {
            Some(DeferredSQLError {
                sqlstate: payload(self.binary.text()?, memory),
                message: payload(self.binary.text()?, memory),
                detail: payload(self.optional_text()?, memory),
                hint: payload(self.optional_text()?, memory),
            })
        } else {
            None
        };
        Ok(IdentitySequenceDeclaration {
            name,
            persistence,
            sequence,
            error,
        })
    }

    fn sequence_option(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<Option<SequenceOptionValue>, SQLError> {
        match self.binary.array::<1>()?[0] {
            0 => Ok(None),
            1 => Ok(Some(SequenceOptionValue::Absent)),
            2 => Ok(Some(SequenceOptionValue::Integer(i64::from_le_bytes(
                self.binary.array()?,
            )))),
            3 => Ok(Some(SequenceOptionValue::Text(payload(
                self.binary.text()?,
                memory,
            )))),
            _ => Err(invalid("identity sequence option")),
        }
    }

    fn sequence_declaration(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<SequenceDeclaration, SQLError> {
        let has_type = self.flag()?;
        let cycle = match self.binary.array::<1>()?[0] {
            0 => None,
            1 => Some(false),
            2 => Some(true),
            _ => return Err(invalid("sequence cycle option")),
        };
        let increment = self.sequence_option(memory)?;
        let max_value = self.sequence_option(memory)?;
        let min_value = self.sequence_option(memory)?;
        let start = self.sequence_option(memory)?;
        let restart = self.sequence_option(memory)?;
        let cache = self.sequence_option(memory)?;
        let owned_count = if self.flag()? {
            Some(self.binary.count()?)
        } else {
            None
        };
        let data_type = has_type
            .then(|| self.column_type().map(|value| payload(value, memory)))
            .transpose()?;
        let owned_by = owned_count
            .map(|count| {
                self.items(count, memory, |input, memory| {
                    Ok(payload(input.binary.text()?, memory))
                })
            })
            .transpose()?;
        Ok(SequenceDeclaration {
            data_type,
            increment,
            cycle,
            max_value,
            min_value,
            start,
            restart,
            cache,
            owned_by,
        })
    }
}
