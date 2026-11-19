//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence declarations retain written metadata without creation or name binding.

use super::{invalid, payload, Fields, MemoryReservation, SQLError};
use crate::ast::{
    AutoIncrement, AutoIncrementKind, AutoIncrementOwner, DeferredSQLError,
    IdentitySequenceDeclaration, IdentitySequenceName, RelationPersistence, SequenceDeclaration,
    SequenceOptionValue,
};

impl Fields<'_> {
    pub(super) async fn auto_increment(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<AutoIncrement, SQLError> {
        let kind = match self.binary.array::<1>().await?[0] {
            0 => AutoIncrementKind::Serial,
            1 => AutoIncrementKind::IdentityAlways,
            2 => AutoIncrementKind::IdentityByDefault,
            3 => AutoIncrementKind::Legacy,
            _ => return Err(invalid("column sequence kind")),
        };
        let sequence = payload(self.optional_text().await?, memory);
        let owner = if self.flag().await? {
            Some(AutoIncrementOwner {
                table: payload(self.binary.text().await?, memory),
                column: payload(self.binary.text().await?, memory),
            })
        } else {
            None
        };
        let declaration = if self.flag().await? {
            memory.grow(size_of::<IdentitySequenceDeclaration>())?;
            Some(Box::new(self.identity_declaration(memory).await?))
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

    async fn identity_declaration(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<IdentitySequenceDeclaration, SQLError> {
        let name = if self.flag().await? {
            Some(IdentitySequenceName {
                schema: payload(self.optional_text().await?, memory),
                name: payload(self.binary.text().await?, memory),
            })
        } else {
            None
        };
        let persistence = match self.binary.array::<1>().await?[0] {
            0 => None,
            1 => Some(RelationPersistence::Permanent),
            2 => Some(RelationPersistence::Unlogged),
            3 => Some(RelationPersistence::Temporary),
            _ => return Err(invalid("identity sequence persistence")),
        };
        let has_error = self.flag().await?;
        let sequence = self.sequence_declaration(memory).await?;
        let error = if has_error {
            Some(DeferredSQLError {
                sqlstate: payload(self.binary.text().await?, memory),
                message: payload(self.binary.text().await?, memory),
                detail: payload(self.optional_text().await?, memory),
                hint: payload(self.optional_text().await?, memory),
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

    async fn sequence_option(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<Option<SequenceOptionValue>, SQLError> {
        match self.binary.array::<1>().await?[0] {
            0 => Ok(None),
            1 => Ok(Some(SequenceOptionValue::Absent)),
            2 => Ok(Some(SequenceOptionValue::Integer(i64::from_le_bytes(
                self.binary.array().await?,
            )))),
            3 => Ok(Some(SequenceOptionValue::Text(payload(
                self.binary.text().await?,
                memory,
            )))),
            _ => Err(invalid("identity sequence option")),
        }
    }

    async fn sequence_declaration(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<SequenceDeclaration, SQLError> {
        let has_type = self.flag().await?;
        let cycle = match self.binary.array::<1>().await?[0] {
            0 => None,
            1 => Some(false),
            2 => Some(true),
            _ => return Err(invalid("sequence cycle option")),
        };
        let increment = self.sequence_option(memory).await?;
        let max_value = self.sequence_option(memory).await?;
        let min_value = self.sequence_option(memory).await?;
        let start = self.sequence_option(memory).await?;
        let restart = self.sequence_option(memory).await?;
        let cache = self.sequence_option(memory).await?;
        let owned_count = if self.flag().await? {
            Some(self.binary.count().await?)
        } else {
            None
        };
        let data_type = if has_type {
            Some(payload(self.column_type().await?, memory))
        } else {
            None
        };
        let owned_by = match owned_count {
            Some(count) => Some(self.texts(count, memory).await?),
            None => None,
        };
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
