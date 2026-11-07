//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Column images preserve sequence provenance and any original declaration without interpretation.

use super::{DefinitionImageReader, Frame, Part};
use crate::{
    ast::{
        AutoIncrement, AutoIncrementKind, AutoIncrementOwner, DeferredSQLError,
        IdentitySequenceDeclaration, IdentitySequenceName, RelationPersistence,
        SequenceDeclaration, SequenceOptionValue,
    },
    SQLError,
};

impl<'a> DefinitionImageReader<'a> {
    pub(super) fn sequence_part(&mut self, part: Part<'a>) -> Result<(), SQLError> {
        match part {
            Part::AutoIncrement(value) => {
                let AutoIncrement {
                    kind,
                    sequence,
                    owner,
                    declaration,
                } = value;
                self.fields.scalar([match kind {
                    AutoIncrementKind::Serial => 0,
                    AutoIncrementKind::IdentityAlways => 1,
                    AutoIncrementKind::IdentityByDefault => 2,
                    AutoIncrementKind::Legacy => 3,
                }]);
                self.fields.optional_text(sequence.as_deref());
                self.fields.flag(owner.is_some());
                if let Some(AutoIncrementOwner { table, column }) = owner {
                    self.fields.text(table);
                    self.fields.text(column);
                }
                self.fields.flag(declaration.is_some());
                if let Some(value) = declaration {
                    self.table_push(Part::IdentityDeclaration(value))?;
                }
            }
            Part::IdentityDeclaration(value) => {
                let IdentitySequenceDeclaration {
                    name,
                    persistence,
                    sequence,
                    error,
                } = value;
                self.fields.flag(name.is_some());
                if let Some(IdentitySequenceName { schema, name }) = name {
                    self.fields.optional_text(schema.as_deref());
                    self.fields.text(name);
                }
                self.fields.scalar([match persistence {
                    None => 0,
                    Some(RelationPersistence::Permanent) => 1,
                    Some(RelationPersistence::Unlogged) => 2,
                    Some(RelationPersistence::Temporary) => 3,
                }]);
                self.fields.flag(error.is_some());
                if let Some(error) = error {
                    self.table_push(Part::DeferredError(error))?;
                }
                self.table_push(Part::SequenceDeclaration(sequence))?;
            }
            Part::SequenceDeclaration(value) => self.column_sequence(value)?,
            Part::DeferredError(value) => {
                let DeferredSQLError {
                    sqlstate,
                    message,
                    detail,
                    hint,
                } = value;
                self.fields.text(sqlstate);
                self.fields.text(message);
                self.fields.optional_text(detail.as_deref());
                self.fields.optional_text(hint.as_deref());
            }
            _ => unreachable!("column sequence image continuation"),
        }
        Ok(())
    }
    fn column_sequence(&mut self, value: &'a SequenceDeclaration) -> Result<(), SQLError> {
        let SequenceDeclaration {
            data_type,
            increment,
            cycle,
            max_value,
            min_value,
            start,
            restart,
            cache,
            owned_by,
        } = value;
        self.fields.flag(data_type.is_some());
        self.fields.scalar([match cycle {
            None => 0,
            Some(false) => 1,
            Some(true) => 2,
        }]);
        for option in [increment, max_value, min_value, start, restart, cache] {
            match option {
                None => self.fields.scalar([0]),
                Some(SequenceOptionValue::Absent) => self.fields.scalar([1]),
                Some(SequenceOptionValue::Integer(value)) => {
                    self.fields.scalar([2]);
                    self.fields.scalar(value.to_le_bytes());
                }
                Some(SequenceOptionValue::Text(value)) => {
                    self.fields.scalar([3]);
                    self.fields.text(value);
                }
            }
        }
        self.fields.flag(owned_by.is_some());
        if let Some(values) = owned_by {
            self.fields.count(values.len());
            self.table_push(Part::Texts(values))?;
        }
        if let Some(value) = data_type {
            self.push(Frame::Type(value))?;
        }
        Ok(())
    }
}
