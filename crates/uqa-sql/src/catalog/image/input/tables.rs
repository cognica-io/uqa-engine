//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native table metadata reconstruction preserves absent fields and never evaluates declarations.

use super::{collections::ImageValue, expressions::payload, invalid, Fields, Input};
use crate::{
    ast::{
        ColumnDef, ConstraintCatalogIdentity, Expr, ForeignKeyAction, ForeignKeyMatch,
        ForeignKeyRef, GeneratedColumn, GeneratedColumnKind, ReferencedPartitionConstraint,
    },
    SQLError,
};
use std::io::Read;
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryReservation},
    CancellationToken,
};

mod attributes;
mod constraints;
mod hierarchy;
mod sequences;
#[cfg(test)]
mod tests;
pub use attributes::{read_dropped_attributes_image, read_dropped_attributes_image_from};
pub use constraints::{
    read_foreign_keys_image, read_foreign_keys_image_from, read_table_checks_image,
    read_table_checks_image_from, read_table_checks_image_with_footprint, read_table_keys_image,
    read_table_keys_image_from,
};
pub use hierarchy::{
    read_table_hierarchy_image, read_table_hierarchy_image_from,
    read_table_hierarchy_image_with_footprint,
};

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
    read_list_with_footprint(reader, memory, cancellation, *b"UQACOL01")
}

/// Resume native columns on a parent input, retaining the original literal footprint.
pub async fn read_columns_image_from(
    input: AsyncImageInput<'_>,
) -> Result<(Budgeted<Vec<ColumnDef>>, usize), SQLError> {
    read_list_from(input, *b"UQACOL01").await
}

fn read_list<T: ImageValue>(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
    magic: [u8; 8],
) -> Result<Budgeted<Vec<T>>, SQLError> {
    Ok(read_list_with_footprint(reader, memory, cancellation, magic)?.0)
}

fn read_list_with_footprint<T: ImageValue>(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
    magic: [u8; 8],
) -> Result<(Budgeted<Vec<T>>, usize), SQLError> {
    let mut input = Input::new(reader, memory, cancellation);
    let count = input.header(magic)?;
    let mut reservation = memory.empty_reservation();
    let values = input.image_values(count, &mut reservation)?;
    input.binary.end()?;
    Ok((Budgeted::new(values, reservation), input.retained_children))
}

impl Input<'_> {
    fn image_values<T: ImageValue>(
        &mut self,
        count: usize,
        memory: &mut MemoryReservation,
    ) -> Result<Vec<T>, SQLError> {
        self.items(count, memory, |input, memory| {
            Ok(payload(T::read_sync(input)?, memory))
        })
    }
}

async fn read_list_from<T: ImageValue>(
    binary: AsyncImageInput<'_>,
    magic: [u8; 8],
) -> Result<(Budgeted<Vec<T>>, usize), SQLError> {
    let mut input = Fields {
        binary,
        retained_children: &mut 0,
    };
    let count = input.header(magic).await?;
    let values = input.image_values(count).await?;
    input.binary.end().await?;
    Ok((values, *input.retained_children))
}

impl Fields<'_> {
    async fn optional_catalog_oid(&mut self) -> Result<Option<i64>, SQLError> {
        if self.flag().await? {
            Ok(Some(self.oid().await?))
        } else {
            Ok(None)
        }
    }

    async fn column_values(
        &mut self,
        memory: &mut MemoryReservation,
        value: &mut ColumnDef,
        present: ColumnPresence,
    ) -> Result<(), SQLError> {
        if present.missing {
            value.missing_value = Some(payload(self.scalar_value().await?, memory));
        }
        if present.auto {
            value.auto_increment = Some(self.auto_increment(memory).await?);
        }
        Ok(())
    }

    async fn column_declaration(
        &mut self,
        memory: &mut MemoryReservation,
        value: &mut ColumnDef,
    ) -> Result<ColumnPresence, SQLError> {
        let present = self.column_header(memory, value).await?;
        value.ty = payload(self.column_type().await?, memory);
        value.not_null_name = payload(self.optional_text().await?, memory);
        value.not_null_identity = self.optional_catalog_identity().await?;
        value.check_name = payload(self.optional_text().await?, memory);
        value.check_object_id = self.optional_incarnation().await?;
        value.check_catalog_oid = self.optional_catalog_oid().await?;
        value.default_catalog_oid = self.optional_catalog_oid().await?;
        Ok(present)
    }

    async fn column_header(
        &mut self,
        memory: &mut MemoryReservation,
        value: &mut ColumnDef,
    ) -> Result<ColumnPresence, SQLError> {
        value.name = payload(self.binary.text().await?, memory);
        value.collation_oid = self.optional_number().await?;
        value.object_id = self.optional_incarnation().await?;
        value.attribute_number = if self.flag().await? {
            Some(i16::from_le_bytes(self.binary.array().await?))
        } else {
            None
        };
        value.primary_key = self.flag().await?;
        value.not_null = self.flag().await?;
        value.not_null_explicit = self.flag().await?;
        value.not_null_validated = self.flag().await?;
        value.not_null_no_inherit = self.flag().await?;
        value.not_null_is_local = self.flag().await?;
        value.unique = self.flag().await?;
        value.check_enforced = self.flag().await?;
        value.check_validated = self.flag().await?;
        value.check_no_inherit = self.flag().await?;
        value.check_is_local = self.flag().await?;
        let has_missing = self.flag().await?;
        let has_auto = self.flag().await?;
        let has_default = self.flag().await?;
        let has_generated = self.flag().await?;
        let has_check = self.flag().await?;
        let has_reference = self.flag().await?;
        Ok(ColumnPresence {
            missing: has_missing,
            auto: has_auto,
            default: has_default,
            generated: has_generated,
            check: has_check,
            reference: has_reference,
        })
    }

    async fn generated_column(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<GeneratedColumn, SQLError> {
        let kind = match self.binary.array::<1>().await?[0] {
            0 => GeneratedColumnKind::Virtual,
            1 => GeneratedColumnKind::Stored,
            _ => return Err(invalid("generated column kind")),
        };
        let count = self.binary.count().await?;
        memory.grow(size_of::<Expr>())?;
        let expression = Box::new(payload(self.expression().await?, memory));
        let mut dependencies = BudgetedVec::new(self.binary.memory);
        dependencies.reserve(count)?;
        for _ in 0..count {
            self.binary.checkpoint().await?;
            if self.header(super::super::FUNCTION_BINDING).await? != 1 {
                return Err(invalid("generated column binding count"));
            }
            dependencies.push(payload(self.function_binding().await?, memory))?;
        }
        let (function_dependencies, reservation) = dependencies.into_parts();
        memory.absorb(reservation);
        Ok(GeneratedColumn {
            kind,
            expression,
            function_dependencies,
        })
    }

    async fn foreign_action(&mut self) -> Result<ForeignKeyAction, SQLError> {
        match self.binary.array::<1>().await?[0] {
            0 => Ok(ForeignKeyAction::NoAction),
            1 => Ok(ForeignKeyAction::Restrict),
            2 => Ok(ForeignKeyAction::Cascade),
            3 => Ok(ForeignKeyAction::SetNull),
            4 => Ok(ForeignKeyAction::SetDefault),
            _ => Err(invalid("foreign key action")),
        }
    }

    async fn foreign_match(&mut self) -> Result<ForeignKeyMatch, SQLError> {
        match self.binary.array::<1>().await?[0] {
            0 => Ok(ForeignKeyMatch::Simple),
            1 => Ok(ForeignKeyMatch::Full),
            _ => Err(invalid("foreign key match")),
        }
    }

    async fn referenced_partition(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<ReferencedPartitionConstraint, SQLError> {
        Ok(ReferencedPartitionConstraint {
            partition: self.object_id().await?,
            parent: self.optional_incarnation().await?,
            name: payload(self.binary.text().await?, memory),
            catalog_identity: ConstraintCatalogIdentity {
                object_id: self.object_id().await?,
                oid: self.oid().await?,
            },
            validated: self.flag().await?,
        })
    }

    async fn texts(
        &mut self,
        count: usize,
        memory: &mut MemoryReservation,
    ) -> Result<Vec<String>, SQLError> {
        Ok(payload(self.image_values::<String>(count).await?, memory))
    }

    async fn foreign_reference(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<ForeignKeyRef, SQLError> {
        let referenced_index = self.optional_incarnation().await?;
        let referenced_key = payload(self.optional_text().await?, memory);
        let name = payload(self.optional_text().await?, memory);
        let object_id = self.optional_incarnation().await?;
        let catalog_identity = self.optional_catalog_identity().await?;
        let table = payload(self.binary.text().await?, memory);
        let column = payload(self.optional_text().await?, memory);
        let on_update = self.foreign_action().await?;
        let on_delete = self.foreign_action().await?;
        let match_type = self.foreign_match().await?;
        let enforced = self.flag().await?;
        let validated = self.flag().await?;
        let deferrable = self.flag().await?;
        let initially_deferred = self.flag().await?;
        let period = self.flag().await?;
        let count = self.binary.count().await?;
        let referenced_partitions = payload(
            self.image_values::<ReferencedPartitionConstraint>(count)
                .await?,
            memory,
        );
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

#[derive(Clone, Copy)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Each field preserves an independent presence tag in the native column format"
)]
struct ColumnPresence {
    missing: bool,
    auto: bool,
    default: bool,
    generated: bool,
    check: bool,
    reference: bool,
}

enum ColumnStage {
    Declaration,
    Values,
    Default,
    Generated,
    Check,
    Reference,
    Complete,
}

struct ColumnInput {
    value: ColumnDef,
    memory: MemoryReservation,
    present: ColumnPresence,
    stage: ColumnStage,
}

impl ColumnInput {
    fn new(memory: &MemoryBudget) -> Self {
        Self {
            value: ColumnDef::nullable("", crate::ast::ColumnType::Text),
            memory: memory.empty_reservation(),
            present: ColumnPresence {
                missing: false,
                auto: false,
                default: false,
                generated: false,
                check: false,
                reference: false,
            },
            stage: ColumnStage::Declaration,
        }
    }

    async fn advance(&mut self, input: &mut Fields<'_>) -> Result<(), SQLError> {
        self.stage = match self.stage {
            ColumnStage::Declaration => {
                self.present = input
                    .column_declaration(&mut self.memory, &mut self.value)
                    .await?;
                ColumnStage::Values
            }
            ColumnStage::Values => {
                input
                    .column_values(&mut self.memory, &mut self.value, self.present)
                    .await?;
                ColumnStage::Default
            }
            ColumnStage::Default | ColumnStage::Check => {
                let (present, target, next) = if matches!(self.stage, ColumnStage::Default) {
                    (
                        self.present.default,
                        &mut self.value.default,
                        ColumnStage::Generated,
                    )
                } else {
                    (
                        self.present.check,
                        &mut self.value.check,
                        ColumnStage::Reference,
                    )
                };
                if present {
                    *target = Some(payload(input.expression().await?, &mut self.memory));
                }
                next
            }
            ColumnStage::Generated => {
                if self.present.generated {
                    self.value.generated = Some(input.generated_column(&mut self.memory).await?);
                }
                ColumnStage::Check
            }
            ColumnStage::Reference => {
                if self.present.reference {
                    self.value.references = Some(input.foreign_reference(&mut self.memory).await?);
                }
                ColumnStage::Complete
            }
            ColumnStage::Complete => return Err(invalid("completed column input")),
        };
        Ok(())
    }

    fn complete(&self) -> bool {
        matches!(self.stage, ColumnStage::Complete)
    }

    fn finish(self) -> Result<Budgeted<ColumnDef>, SQLError> {
        if !self.complete() {
            return Err(invalid("incomplete column input"));
        }
        Ok(Budgeted::new(self.value, self.memory))
    }
}

impl ImageValue for ColumnDef {
    async fn read(input: &mut Fields<'_>) -> Result<Budgeted<Self>, SQLError> {
        let mut column = ColumnInput::new(input.binary.memory);
        while !column.complete() {
            column.advance(input).await?;
        }
        column.finish()
    }

    fn read_sync(input: &mut Input<'_>) -> Result<Budgeted<Self>, SQLError> {
        let mut column = ColumnInput::new(input.binary.memory);
        while !column.complete() {
            input.fields(async |mut fields| column.advance(&mut fields).await)?;
        }
        column.finish()
    }
}

impl ImageValue for ReferencedPartitionConstraint {
    async fn read(input: &mut Fields<'_>) -> Result<Budgeted<Self>, SQLError> {
        let mut memory = input.binary.memory.empty_reservation();
        let value = input.referenced_partition(&mut memory).await?;
        Ok(Budgeted::new(value, memory))
    }
}
