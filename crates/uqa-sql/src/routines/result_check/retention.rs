//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admit result descriptions before copying their owned names and type payloads.

use super::{ColumnType, SQLFunctionResultColumn, SQLFunctionResultLayout};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    ValueRetentionError,
};

pub(crate) fn copy_types(
    types: &[Option<ColumnType>],
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<Option<ColumnType>>>, ValueRetentionError> {
    let mut result = ProductionVec::new(*control);
    result.reserve(types.len())?;
    for ty in types {
        let (ty, memory) = match ty {
            Some(ty) => {
                let (ty, memory) = ty.clone_with_control(control)?.into_parts();
                (Some(ty), memory)
            }
            None => (None, control.empty_reservation()),
        };
        result.push_produced(control.finish(ty, memory)?)?;
    }
    result.finish()
}

fn copy_columns(
    columns: &[SQLFunctionResultColumn],
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<SQLFunctionResultColumn>>, ValueRetentionError> {
    let mut result = ProductionVec::new(*control);
    result.reserve(columns.len())?;
    for column in columns {
        let name = control.copy_text(&column.name)?;
        let ty = column.ty.clone_with_control(control)?;
        let (name, names) = name.into_parts();
        let (ty, types) = ty.into_parts();
        result.push_produced(control.finish(
            SQLFunctionResultColumn { name, ty },
            control.combine(names, types),
        )?)?;
    }
    result.finish()
}

impl SQLFunctionResultLayout {
    pub fn clone_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, ValueRetentionError> {
        let declared = self.declared_type.clone_with_control(control)?;
        let source = self
            .source_record
            .as_ref()
            .map(|types| copy_types(types, control))
            .transpose()?;
        let columns = self
            .columns
            .as_ref()
            .map(|columns| copy_columns(columns, control))
            .transpose()?;
        let (declared_type, mut memory) = declared.into_parts();
        let source_record = source.map(|source| {
            let (source, lease) = source.into_parts();
            memory = control.combine(memory.take(), lease);
            source
        });
        let columns = columns.map(|columns| {
            let (columns, lease) = columns.into_parts();
            memory = control.combine(memory.take(), lease);
            columns
        });
        control.finish(
            Self {
                kind: self.kind,
                declared_type,
                source_record,
                columns,
            },
            memory,
        )
    }
}
