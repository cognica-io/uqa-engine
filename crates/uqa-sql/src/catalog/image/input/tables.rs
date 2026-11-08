//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native table metadata reconstruction preserves absent fields and never evaluates declarations.

use super::{expressions::payload, invalid, Input};
use crate::{
    ast::{
        ColumnDef, ConstraintCatalogIdentity, Expr, ForeignKeyAction, ForeignKeyMatch,
        ForeignKeyRef, GeneratedColumn, GeneratedColumnKind, ReferencedPartitionConstraint,
    },
    SQLError,
};
use std::io::Read;
use uqa_core::{
    memory::{Budgeted, MemoryBudget, MemoryReservation},
    CancellationToken,
};

mod attributes;
mod constraints;
mod hierarchy;
mod sequences;
pub use attributes::read_dropped_attributes_image;
pub use constraints::{
    read_foreign_keys_image, read_table_checks_image, read_table_checks_image_with_footprint,
    read_table_keys_image,
};
pub use hierarchy::read_table_hierarchy_image;

/// Reconstruct ordered native columns under one retained allowance. Scalar payloads, physical identities and declaration order remain unchanged; relation-wide validation belongs to the enclosing restore owner.
pub fn read_columns_image(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<Vec<ColumnDef>>, SQLError> {
    Ok(read_columns_image_with_footprint(reader, memory, cancellation)?.0)
}

/// Reconstruct native columns and report separately owned literal bytes for independent capture admission. Their original constructor leases stay with the literals.
pub fn read_columns_image_with_footprint(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(Budgeted<Vec<ColumnDef>>, usize), SQLError> {
    read_list_with_footprint(
        reader,
        memory,
        cancellation,
        *b"UQACOL01",
        Input::column_definition,
    )
}

fn read_list<'a, T>(
    reader: &'a mut dyn Read,
    memory: &'a MemoryBudget,
    cancellation: &'a CancellationToken,
    magic: [u8; 8],
    item: impl FnMut(&mut Input<'a>, &mut MemoryReservation) -> Result<T, SQLError>,
) -> Result<Budgeted<Vec<T>>, SQLError> {
    Ok(read_list_with_footprint(reader, memory, cancellation, magic, item)?.0)
}

fn read_list_with_footprint<'a, T>(
    reader: &'a mut dyn Read,
    memory: &'a MemoryBudget,
    cancellation: &'a CancellationToken,
    magic: [u8; 8],
    item: impl FnMut(&mut Input<'a>, &mut MemoryReservation) -> Result<T, SQLError>,
) -> Result<(Budgeted<Vec<T>>, usize), SQLError> {
    let mut input = Input::new(reader, memory, cancellation);
    let count = input.header(magic)?;
    let mut reservation = memory.empty_reservation();
    let values = input.items(count, &mut reservation, item)?;
    input.binary.end()?;
    Ok((Budgeted::new(values, reservation), input.retained_children))
}

impl Input<'_> {
    fn optional_catalog_oid(&mut self) -> Result<Option<i64>, SQLError> {
        if self.flag()? {
            Ok(Some(self.oid()?))
        } else {
            Ok(None)
        }
    }

    fn column_definition(&mut self, memory: &mut MemoryReservation) -> Result<ColumnDef, SQLError> {
        let name = payload(self.binary.text()?, memory);
        let collation_oid = self.optional_number()?;
        let object_id = self.optional_incarnation()?;
        let attribute_number = if self.flag()? {
            Some(i16::from_le_bytes(self.binary.array()?))
        } else {
            None
        };
        let primary_key = self.flag()?;
        let not_null = self.flag()?;
        let not_null_explicit = self.flag()?;
        let not_null_validated = self.flag()?;
        let not_null_no_inherit = self.flag()?;
        let not_null_is_local = self.flag()?;
        let unique = self.flag()?;
        let check_enforced = self.flag()?;
        let check_validated = self.flag()?;
        let check_no_inherit = self.flag()?;
        let check_is_local = self.flag()?;
        let has_missing = self.flag()?;
        let has_auto = self.flag()?;
        let has_default = self.flag()?;
        let has_generated = self.flag()?;
        let has_check = self.flag()?;
        let has_reference = self.flag()?;
        let ty = payload(self.column_type()?, memory);
        let not_null_name = payload(self.optional_text()?, memory);
        let not_null_identity = self.optional_catalog_identity()?;
        let check_name = payload(self.optional_text()?, memory);
        let check_object_id = self.optional_incarnation()?;
        let check_catalog_oid = self.optional_catalog_oid()?;
        let default_catalog_oid = self.optional_catalog_oid()?;
        let missing_value = has_missing
            .then(|| self.scalar_value().map(|value| payload(value, memory)))
            .transpose()?;
        let auto_increment = has_auto.then(|| self.auto_increment(memory)).transpose()?;
        let default = has_default
            .then(|| self.expression().map(|value| payload(value, memory)))
            .transpose()?;
        let generated = has_generated
            .then(|| self.generated_column(memory))
            .transpose()?;
        let check = has_check
            .then(|| self.expression().map(|value| payload(value, memory)))
            .transpose()?;
        let references = has_reference
            .then(|| self.foreign_reference(memory))
            .transpose()?;
        Ok(ColumnDef {
            name,
            ty,
            collation_oid,
            object_id,
            attribute_number,
            missing_value,
            primary_key,
            not_null,
            not_null_explicit,
            not_null_name,
            not_null_identity,
            not_null_validated,
            not_null_no_inherit,
            not_null_is_local,
            auto_increment,
            unique,
            default,
            generated,
            default_catalog_oid,
            check,
            check_name,
            check_enforced,
            check_validated,
            check_no_inherit,
            check_is_local,
            check_object_id,
            check_catalog_oid,
            references,
        })
    }

    fn generated_column(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<GeneratedColumn, SQLError> {
        let kind = match self.binary.array::<1>()?[0] {
            0 => GeneratedColumnKind::Virtual,
            1 => GeneratedColumnKind::Stored,
            _ => return Err(invalid("generated column kind")),
        };
        let count = self.binary.count()?;
        memory.grow(size_of::<Expr>())?;
        let expression = Box::new(payload(self.expression()?, memory));
        let function_dependencies = self.items(count, memory, |input, memory| {
            if input.header(super::super::FUNCTION_BINDING)? != 1 {
                return Err(invalid("generated column binding count"));
            }
            Ok(payload(input.function_binding()?, memory))
        })?;
        Ok(GeneratedColumn {
            kind,
            expression,
            function_dependencies,
        })
    }

    fn foreign_action(&mut self) -> Result<ForeignKeyAction, SQLError> {
        match self.binary.array::<1>()?[0] {
            0 => Ok(ForeignKeyAction::NoAction),
            1 => Ok(ForeignKeyAction::Restrict),
            2 => Ok(ForeignKeyAction::Cascade),
            3 => Ok(ForeignKeyAction::SetNull),
            4 => Ok(ForeignKeyAction::SetDefault),
            _ => Err(invalid("foreign key action")),
        }
    }

    fn foreign_match(&mut self) -> Result<ForeignKeyMatch, SQLError> {
        match self.binary.array::<1>()?[0] {
            0 => Ok(ForeignKeyMatch::Simple),
            1 => Ok(ForeignKeyMatch::Full),
            _ => Err(invalid("foreign key match")),
        }
    }

    fn referenced_partition(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<ReferencedPartitionConstraint, SQLError> {
        Ok(ReferencedPartitionConstraint {
            partition: self.object_id()?,
            parent: self.optional_incarnation()?,
            name: payload(self.binary.text()?, memory),
            catalog_identity: ConstraintCatalogIdentity {
                object_id: self.object_id()?,
                oid: self.oid()?,
            },
            validated: self.flag()?,
        })
    }

    fn texts(
        &mut self,
        count: usize,
        memory: &mut MemoryReservation,
    ) -> Result<Vec<String>, SQLError> {
        self.items(count, memory, |input, memory| {
            Ok(payload(input.binary.text()?, memory))
        })
    }

    fn foreign_reference(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<ForeignKeyRef, SQLError> {
        let referenced_index = self.optional_incarnation()?;
        let referenced_key = payload(self.optional_text()?, memory);
        let name = payload(self.optional_text()?, memory);
        let object_id = self.optional_incarnation()?;
        let catalog_identity = self.optional_catalog_identity()?;
        let table = payload(self.binary.text()?, memory);
        let column = payload(self.optional_text()?, memory);
        let on_update = self.foreign_action()?;
        let on_delete = self.foreign_action()?;
        let match_type = self.foreign_match()?;
        let enforced = self.flag()?;
        let validated = self.flag()?;
        let deferrable = self.flag()?;
        let initially_deferred = self.flag()?;
        let period = self.flag()?;
        let count = self.binary.count()?;
        let referenced_partitions = self.items(count, memory, Self::referenced_partition)?;
        Ok(ForeignKeyRef {
            referenced_index,
            referenced_key,
            name,
            object_id,
            catalog_identity,
            table,
            column,
            on_update,
            on_delete,
            match_type,
            enforced,
            validated,
            deferrable,
            initially_deferred,
            period,
            referenced_partitions,
        })
    }
}
