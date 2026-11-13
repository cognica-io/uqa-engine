//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use parking_lot::Mutex;
use std::sync::Arc;
use uqa_sql::{
    ast::{ColumnDef, ColumnType, Expr, WindowCallModifiers, WindowSpec},
    catalog::{
        table_definition::{SharedTableDefinition, TableDefinition},
        table_metadata::{SharedTableMetadata, TableMetadata},
    },
};

#[derive(Clone)]
struct Table {
    fields: Arc<Mutex<TableMetadata<Vec<String>>>>,
    columns: SharedTableDefinition<Vec<ColumnDef>>,
    declared: bool,
}

impl AnalyzerTableDefinition for Table {
    fn fields(&self) -> SharedTableMetadata<Vec<String>> {
        self.fields.lock().snapshot()
    }
    fn columns_declared(&self) -> bool {
        self.declared
    }
    fn columns(&self) -> SharedTableDefinition<Vec<ColumnDef>> {
        self.columns.clone()
    }
}

impl GinAnalyzerTable for Table {
    fn register_field(&self, field: &str) -> StorageBackendResult<()> {
        TableMetadata::write(self.fields.lock()).push(field.to_owned());
        Ok(())
    }
}

struct Tables {
    table: Table,
    visits: Mutex<Vec<String>>,
}

impl GinAnalyzerTables for Tables {
    type Table = Table;
    fn table(&self, name: &str) -> StorageBackendResult<Option<Table>> {
        self.visits.lock().push(name.to_owned());
        Ok((name == "public.docs").then(|| self.table.clone()))
    }
}

fn tables() -> Tables {
    Tables {
        table: Table {
            fields: Arc::new(Mutex::new(TableMetadata::new(Vec::new()))),
            columns: TableDefinition::new(&vec![
                ColumnDef::nullable("body", ColumnType::Text),
                ColumnDef::nullable("count", ColumnType::Integer),
            ])
            .unwrap()
            .shared(),
            declared: true,
        },
        visits: Mutex::new(Vec::new()),
    }
}

fn row(columns: &str, parameters: &str) -> CatalogIndexRow {
    CatalogIndexRow {
        relation: RelationIdentity::new("public", "text_index"),
        index_type: "gIn".into(),
        table_name: "docs".into(),
        columns_json: columns.into(),
        parameters_json: parameters.into(),
        definition_json: Some("ignored by analyzer ownership".into()),
    }
}

#[test]
fn gin_owners_preserve_full_expression_acceptance_and_sorted_parameter_selection() {
    let tables = tables();
    let keys = serde_json::to_string(&vec![
        IndexKey::Expression(Box::new(Expr::WindowCall {
            name: "count".into(),
            args: Vec::new(),
            spec: Box::new(WindowSpec {
                raw_definition: None,
                definition: None,
                reference: None,
                partition_by: Vec::new(),
                order_by: Vec::new(),
                frame: None,
            }),
            filter: None,
            modifiers: WindowCallModifiers::default(),
        })),
        IndexKey::Column("body".into()),
        IndexKey::Column("body".into()),
    ])
    .unwrap();
    let owners = restore_rows(
        vec![row(
            &keys,
            r#"{"analyzer":"later","ANALYZER":"  first  ","unrelated":"kept"}"#,
        )],
        &tables,
    )
    .unwrap();
    assert_eq!(
        owners,
        BTreeMap::from([(("public.docs".into(), "body".into()), "first".into())])
    );
    assert_eq!(tables.table.fields().as_slice(), ["body"]);
    assert_eq!(*tables.visits.lock(), ["public.docs"]);
}

#[test]
fn gin_owners_preserve_filter_table_key_parameter_and_column_error_order() {
    let tables = tables();
    let mut ignored = row("invalid keys", "invalid parameters");
    ignored.index_type = "btree".into();
    ignored.table_name = "missing".into();
    assert!(restore_rows(vec![ignored], &tables).unwrap().is_empty());
    assert!(tables.visits.lock().is_empty());
    let mut missing = row("invalid keys", "invalid parameters");
    missing.table_name = "missing".into();
    assert_eq!(
        restore_rows(vec![missing], &tables)
            .unwrap_err()
            .to_string(),
        "GIN analyzer owner references a missing table"
    );
    let keys = r#"["body", {"InvalidExpression":null}]"#;
    assert_eq!(
        restore_rows(vec![row(keys, "invalid parameters")], &tables)
            .unwrap_err()
            .to_string(),
        StorageBackendError::from(serde_json::from_str::<Vec<IndexKey>>(keys).unwrap_err())
            .to_string()
    );
    assert!(tables.table.fields().is_empty());
    let parameters = r#"{"analyzer":"good","ignored":false}"#;
    assert_eq!(
        restore_rows(vec![row(r#"["count"]"#, parameters)], &tables)
            .unwrap_err()
            .to_string(),
        StorageBackendError::from(
            serde_json::from_str::<BTreeMap<String, String>>(parameters).unwrap_err(),
        )
        .to_string()
    );
    assert!(tables.table.fields().is_empty());
}

#[test]
fn gin_owners_publish_fields_before_declared_type_and_competing_owner_errors() {
    let tables = tables();
    assert_eq!(
        restore_rows(vec![row(r#"["body","count"]"#, "{}")], &tables)
            .unwrap_err()
            .to_string(),
        "set_table_analyzer: column `public.docs`.`count` must be TEXT, got Integer"
    );
    assert_eq!(tables.table.fields().as_slice(), ["body", "count"]);
    let tables = self::tables();
    assert_eq!(
        restore_rows(
            vec![
                row(r#"["body"]"#, r#"{"analyzer":"first"}"#),
                row(r#"["body"]"#, r#"{"analyzer":"second"}"#),
            ],
            &tables,
        )
        .unwrap_err()
        .to_string(),
        "GIN definitions have competing analyzer owners"
    );
    assert_eq!(tables.table.fields().as_slice(), ["body"]);
    let mut tables = self::tables();
    tables.table.declared = false;
    assert!(restore_rows(vec![row(r#"["undeclared"]"#, "{}")], &tables)
        .unwrap()
        .is_empty());
    assert_eq!(tables.table.fields().as_slice(), ["undeclared"]);
}
