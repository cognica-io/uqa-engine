//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::context::{RetrievalIndexState, TextIndexRead, VectorIndexRead};
use super::validate_vector_query;
use std::{collections::BTreeMap, sync::Arc};
use uqa_sql::{ast::ColumnDef, SQLError};
use uqa_storage::{vector_index::VectorIndexes, MemoryVectorIndex, VectorIndex};

struct CapturedTable {
    columns: Arc<Vec<ColumnDef>>,
    vectors: VectorIndexes,
}

impl RetrievalIndexState for CapturedTable {
    fn columns(&self) -> Arc<Vec<ColumnDef>> {
        Arc::clone(&self.columns)
    }

    fn inverted_index(&self) -> TextIndexRead<'_> {
        panic!("vector validation must not inspect text indexes")
    }

    fn vector_indexes(&self) -> VectorIndexRead<'_> {
        Box::new(&self.vectors)
    }
}

fn table(declaration: Option<&str>, dimensions: Option<u32>) -> CapturedTable {
    let columns = declaration.map_or_else(Vec::new, |declaration| {
        let uqa_sql::Statement::CreateTable(table) =
            uqa_sql::compile(&format!("CREATE TABLE captured(embedding {declaration})"))
                .unwrap()
                .remove(0)
        else {
            panic!("fixture must declare a table")
        };
        table.columns
    });
    let mut vectors = BTreeMap::<String, Box<dyn VectorIndex>>::new();
    if let Some(dimensions) = dimensions {
        vectors.insert(
            "embedding".into(),
            Box::new(MemoryVectorIndex::new(dimensions)),
        );
    }
    CapturedTable {
        columns: Arc::new(columns),
        vectors: vectors.into(),
    }
}

#[test]
fn captured_vector_and_tensor_dimensions_define_validation() {
    for declaration in [None, Some("vector(3)"), Some("tensor(3)")] {
        let captured = table(declaration, Some(3));
        validate_vector_query(&captured, "embedding", &[0.0, 1.0, 0.0]).unwrap();
        assert!(matches!(
            validate_vector_query(&captured, "embedding", &[1.0, 0.0]),
            Err(SQLError::TypeMismatch(message)) if message == "vector query for \"embedding\" has 2 dimensions, expected 3"
        ));
        for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(matches!(
                validate_vector_query(&captured, "embedding", &[0.0, invalid, 0.0]),
                Err(SQLError::TypeMismatch(message)) if message == "vector query for \"embedding\" must contain only finite values"
            ));
        }
    }
}

#[test]
fn captured_schema_and_index_errors_precede_query_errors() {
    let invalid_query = [f32::NAN];
    assert!(matches!(
        validate_vector_query(&table(Some("text"), None), "embedding", &invalid_query),
        Err(SQLError::TypeMismatch(message)) if message.contains("requires a VECTOR or TENSOR field")
    ));
    assert!(matches!(
        validate_vector_query(&table(Some("vector(3)"), None), "embedding", &invalid_query),
        Err(SQLError::Unsupported(message)) if message == "vector field \"embedding\" has no physical vector index"
    ));
    assert!(matches!(
        validate_vector_query(&table(None, None), "embedding", &invalid_query),
        Err(SQLError::UnknownColumn(name)) if name == "embedding"
    ));
    assert!(matches!(
        validate_vector_query(&table(Some("vector(3)"), Some(2)), "embedding", &invalid_query),
        Err(SQLError::Internal(message)) if message == "vector schema for \"embedding\" declares 3 dimensions but its index has 2"
    ));
    assert!(matches!(
        validate_vector_query(&table(Some("vector(3)"), Some(3)), "embedding", &invalid_query),
        Err(SQLError::TypeMismatch(message)) if message == "vector query for \"embedding\" has 1 dimensions, expected 3"
    ));
}
