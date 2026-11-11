//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete column and authority preparation precedes any persistent table hydration.

use super::{restore_security_from_input, table_security_error};
use uqa_core::{
    image_io::{AsyncImageInput, ImageDecodeWindow, ImageDecoder},
    memory::{Budgeted, BudgetedVec, MemoryReservation, ProductionControl},
    RelationIdentity,
};
use uqa_sql::{
    ast::ColumnDef,
    catalog::{
        roles::RoleDefinitions, security::BoundTableSecurity, table_definition::TableDefinition,
    },
};
use uqa_storage::{
    catalog::table_load::{CatalogTable, CatalogTableComponents, CatalogTables},
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult, VectorFieldSchema,
};

/// Original physical fields accompany one decoded column generation and validated authority. Analyzer, constraint and physical-store interpretation remains after the complete preparation pass.
pub struct PreparedTableDeclaration {
    pub relation: Budgeted<RelationIdentity>,
    pub security: Budgeted<BoundTableSecurity>,
    pub object_id: [u8; 16],
    pub storage_generation: [u8; 16],
    pub analyzer_json: Budgeted<String>,
    pub fts_fields: Budgeted<Vec<String>>,
    pub vector_fields: Budgeted<Vec<VectorFieldSchema>>,
    pub columns: TableDefinition<Vec<ColumnDef>>,
    pub constraints_json: Budgeted<String>,
}

pub type PreparedTableDeclarations = BudgetedVec<PreparedTableDeclaration>;

pub struct TableDeclarationInput {
    input: ImageDecoder<PreparedTableDeclarations, StorageBackendError>,
}

struct Source {
    rows: std::vec::IntoIter<CatalogTable>,
    _memory: MemoryReservation,
}

impl TableDeclarationInput {
    /// Consume the completed physical catalog in its original order. The role generation is immutable and retains its existing native owners. Stored JSON decoding remains bounded by the physical catalog's selected record limit; field traversal and authority validation use the continuation's work windows.
    pub fn new(
        tables: CatalogTables,
        roles: RoleDefinitions,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        if !tables.budget().shares_allowance(control.memory()) {
            return Err(StorageBackendError::Other(
                "table declaration input requires its original allowance".into(),
            ));
        }
        let retained = control.clone();
        let input = ImageDecoder::new(
            control.memory(),
            control.cancellation(),
            |mut reader| async move {
                let (tables, memory) = tables.into_parts();
                let mut source = Source {
                    rows: tables.into_iter(),
                    _memory: memory,
                };
                let mut output = BudgetedVec::new(retained.memory());
                for table in source.rows.by_ref() {
                    let fields = table.into_components_from(reader.input()).await?;
                    let table = prepare(fields, &roles, &retained, &mut reader.input()).await?;
                    output.push(table)?;
                }
                drop(source);
                retained.check()?;
                Ok(output)
            },
        )?;
        Ok(Self { input })
    }

    /// Return the entire prepared catalog only after every column declaration and authority succeeds. Failure, cancellation or dropping the input retires all previously prepared and unread rows.
    pub fn advance(
        &mut self,
        steps: usize,
    ) -> StorageBackendResult<Option<PreparedTableDeclarations>> {
        self.input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
    }
}

async fn prepare(
    fields: CatalogTableComponents,
    roles: &RoleDefinitions,
    retained: &StorageReadControl,
    input: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<PreparedTableDeclaration> {
    let CatalogTableComponents {
        relation,
        security,
        object_id,
        storage_generation,
        analyzer_json,
        fts_fields,
        vector_fields,
        columns_json,
        constraints_json,
    } = fields;
    input.checkpoint().await?;
    let columns = TableDefinition::read_stored_json(
        if columns_json.is_empty() {
            "[]"
        } else {
            &columns_json
        },
        retained.memory(),
        retained.cancellation(),
        input.cancellation,
    )
    .map_err(column_error)?;
    drop(columns_json);
    let control = ProductionControl::new(
        retained.memory(),
        retained.cancellation(),
        input.cancellation,
    );
    let security = restore_security_from_input(
        security,
        Some(columns.iter().map(column_name)),
        roles,
        &control,
        input,
    )
    .await
    .map_err(|error| table_security_error(&relation, error))?;
    Ok(PreparedTableDeclaration {
        relation,
        security,
        object_id,
        storage_generation,
        analyzer_json,
        fts_fields,
        vector_fields,
        columns,
        constraints_json,
    })
}

fn column_name(column: &ColumnDef) -> &str {
    &column.name
}

fn column_error(error: uqa_sql::SQLError) -> StorageBackendError {
    match error {
        uqa_sql::SQLError::Cancelled(error) => StorageBackendError::Cancelled(error),
        error => StorageBackendError::backend("stored table columns", error),
    }
}

#[cfg(test)]
mod tests;
