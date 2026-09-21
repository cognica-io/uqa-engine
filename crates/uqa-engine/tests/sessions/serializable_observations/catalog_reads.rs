//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public catalog queries enter the same transaction as subsequent row reads.

use super::admission::{observed_fixture, participant, pending_snapshot};
use super::*;
use uqa_core::Value;
use uqa_engine::{SQLFunctionOptions, SQLFunctionVolatility};
use uqa_sql::SQLError;
use uqa_storage::mvcc::SerializableTransactionId;
use uqa_storage::StorageBackendError;

#[derive(Clone, Copy, Debug)]
pub(crate) enum CatalogRead {
    HasTable,
    TryHasTable,
    Columns,
    TryColumns,
    HasColumn,
    TryHasColumn,
    Names,
    Description,
    TryDescription,
    Indexes,
    HasIndex,
    Index,
    HasSchema,
    HasNamespace,
    Schemas,
    SchemaTables,
    CurrentSchema,
    CurrentSchemas,
    Sequences,
    SequenceSnapshot,
    TrySequenceSnapshot,
    SequenceState,
    View,
    Views,
    Analyzers,
    AnalyzeText,
    FieldAnalyzer,
    AnalyzerConfiguration,
    ForeignServer,
    ForeignTable,
    ForeignServers,
    ForeignTables,
    ForeignColumns,
    Default,
    TryDefault,
    Checks,
    TryChecks,
    CheckDefinitions,
    ForeignKeys,
    TryForeignKeys,
    Referrers,
    TryReferrers,
    UniqueColumns,
    TryUniqueColumns,
    Keys,
    TryKeys,
    Model,
    PredictFeatures,
}

impl CatalogRead {
    pub(crate) const ALL: [Self; 48] = [
        Self::HasTable,
        Self::TryHasTable,
        Self::Columns,
        Self::TryColumns,
        Self::HasColumn,
        Self::TryHasColumn,
        Self::Names,
        Self::Description,
        Self::TryDescription,
        Self::Indexes,
        Self::HasIndex,
        Self::Index,
        Self::HasSchema,
        Self::HasNamespace,
        Self::Schemas,
        Self::SchemaTables,
        Self::CurrentSchema,
        Self::CurrentSchemas,
        Self::Sequences,
        Self::SequenceSnapshot,
        Self::TrySequenceSnapshot,
        Self::SequenceState,
        Self::View,
        Self::Views,
        Self::Analyzers,
        Self::AnalyzeText,
        Self::FieldAnalyzer,
        Self::AnalyzerConfiguration,
        Self::ForeignServer,
        Self::ForeignTable,
        Self::ForeignServers,
        Self::ForeignTables,
        Self::ForeignColumns,
        Self::Default,
        Self::TryDefault,
        Self::Checks,
        Self::TryChecks,
        Self::CheckDefinitions,
        Self::ForeignKeys,
        Self::TryForeignKeys,
        Self::Referrers,
        Self::TryReferrers,
        Self::UniqueColumns,
        Self::TryUniqueColumns,
        Self::Keys,
        Self::TryKeys,
        Self::Model,
        Self::PredictFeatures,
    ];

    pub(crate) fn read(self, engine: &Engine) -> Result<(), CatalogError> {
        match self {
            Self::HasTable => ignore_value(engine.has_table("left_t")),
            Self::TryHasTable => ignore_value(engine.try_has_table("left_t")),
            Self::Columns => ignore_value(engine.table_columns("left_t")),
            Self::TryColumns => ignore_value(engine.try_table_columns("left_t")),
            Self::HasColumn => ignore_value(engine.table_has_column("left_t", "v")),
            Self::TryHasColumn => ignore_value(engine.try_table_has_column("left_t", "v")),
            Self::Names => ignore_value(engine.table_names()),
            Self::Description => ignore_value(engine.describe_table("left_t")),
            Self::TryDescription => ignore_value(engine.try_describe_table("left_t")),
            Self::Indexes => ignore_value(engine.list_catalog_indexes()),
            Self::HasIndex => ignore_value(engine.has_catalog_index("left_t_pkey")),
            Self::Index => ignore_value(engine.catalog_index("left_t_pkey")),
            Self::HasSchema => ignore_value(engine.has_schema("public")),
            Self::HasNamespace => ignore_value(engine.has_namespace("public")),
            Self::Schemas => ignore_value(engine.list_schemas()),
            Self::SchemaTables => ignore_value(engine.tables_in_schema("public")),
            Self::CurrentSchema => ignore_value(engine.current_schema_name()),
            Self::CurrentSchemas => ignore_value(engine.current_schema_names(true)),
            Self::Sequences => ignore_value(engine.list_sequences()),
            Self::SequenceSnapshot => ignore_value(engine.sequences_snapshot()),
            Self::TrySequenceSnapshot => ignore_value(engine.try_sequences_snapshot()),
            Self::SequenceState => ignore_value(engine.sequence_state("catalog_s")),
            Self::View => ignore_value(engine.view("catalog_v")),
            Self::Views => ignore_value(engine.list_views()),
            Self::Analyzers => ignore_value(engine.list_named_analyzers()),
            Self::AnalyzeText => ignore_value(engine.analyze_text("standard", "catalog read")),
            Self::FieldAnalyzer => ignore_value(engine.table_field_analyzer("left_t", "v")),
            Self::AnalyzerConfiguration => {
                ignore_value(engine.get_table_analyzer("left_t", "v", "both"))
            }
            Self::ForeignServer => ignore_value(engine.foreign_server("catalog_server")),
            Self::ForeignTable => ignore_value(engine.foreign_table("catalog_foreign")),
            Self::ForeignServers => ignore_value(engine.list_foreign_servers()),
            Self::ForeignTables => ignore_value(engine.list_foreign_tables()),
            Self::ForeignColumns => ignore_value(engine.foreign_table_columns("catalog_foreign")),
            Self::Default => ignore_value(engine.column_default_expr("left_t", "v")),
            Self::TryDefault => ignore_value(engine.try_column_default_expr("left_t", "v")),
            Self::Checks => ignore_value(engine.check_constraints("left_t")),
            Self::TryChecks => ignore_value(engine.try_check_constraints("left_t")),
            Self::CheckDefinitions => {
                ignore_value(engine.try_check_constraint_definitions("left_t"))
            }
            Self::ForeignKeys => ignore_value(engine.foreign_keys("left_t")),
            Self::TryForeignKeys => ignore_value(engine.try_foreign_keys("left_t")),
            Self::Referrers => ignore_value(engine.referrers_to("left_t")),
            Self::TryReferrers => ignore_value(engine.try_referrers_to("left_t")),
            Self::UniqueColumns => ignore_value(engine.unique_columns("left_t")),
            Self::TryUniqueColumns => ignore_value(engine.try_unique_columns("left_t")),
            Self::Keys => ignore_value(engine.key_constraints("left_t")),
            Self::TryKeys => ignore_value(engine.try_key_constraints("left_t")),
            Self::Model => ignore_value(engine.load_model("catalog_model")),
            Self::PredictFeatures => {
                ignore_value(engine.deep_predict_features("catalog_model", &[(1, vec![1.0])]))
            }
        }
    }
}

fn ignore_value<T, E: Into<CatalogError>>(result: Result<T, E>) -> Result<(), CatalogError> {
    result.map(|_| ()).map_err(Into::into)
}

#[derive(Debug)]
pub(crate) enum CatalogError {
    Storage(StorageBackendError),
    Query(SQLError),
    Text(String),
}

impl From<StorageBackendError> for CatalogError {
    fn from(error: StorageBackendError) -> Self {
        Self::Storage(error)
    }
}
impl From<SQLError> for CatalogError {
    fn from(error: SQLError) -> Self {
        Self::Query(error)
    }
}
impl From<String> for CatalogError {
    fn from(error: String) -> Self {
        Self::Text(error)
    }
}
impl CatalogError {
    pub(crate) fn assert_transaction_error(&self, expected: &SQLError) {
        match self {
            Self::Storage(error) => assert_eq!(
                uqa_execution::storage_errors::storage_error("catalog query", error).sqlstate(),
                expected.sqlstate(),
                "{error}"
            ),
            Self::Query(error) => assert_eq!(error.sqlstate(), expected.sqlstate(), "{error}"),
            Self::Text(error) => assert_eq!(error, &expected.to_string()),
        }
    }
}

fn catalog_fixtures() -> (tempfile::TempDir, Vec<Session>) {
    let (directory, sessions) = fixtures();
    for session in &sessions {
        session.sql("CREATE SEQUENCE catalog_s; CREATE VIEW catalog_v AS SELECT 1 AS n; CREATE SERVER catalog_server FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE catalog_foreign (id INTEGER) SERVER catalog_server");
        session
            .engine
            .save_model(
                "catalog_model",
                &uqa_ml::DeepModel {
                    layers: vec![
                        uqa_ml::DeepLayerSpec::Input { dimensions: 1 },
                        uqa_ml::DeepLayerSpec::Dense {
                            weights: vec![1.0],
                            bias: vec![0.0],
                            input_channels: 1,
                            output_channels: 1,
                        },
                        uqa_ml::DeepLayerSpec::Softmax,
                    ],
                    alpha: 0.0,
                    gating: uqa_ml::GatingSpec::None,
                },
            )
            .unwrap();
    }
    (directory, sessions)
}

#[test]
fn first_catalog_query_retains_the_data_snapshot_through_savepoint_undo() {
    let (_directory, sessions) = catalog_fixtures();
    for a in sessions {
        let b = a.sibling();
        for isolation in ["REPEATABLE READ", "SERIALIZABLE"] {
            for read in CatalogRead::ALL {
                a.sql(&format!(
                    "BEGIN ISOLATION LEVEL {isolation}; SAVEPOINT before_read"
                ));
                assert!(participant(&a).is_none());
                b.sql("UPDATE left_t SET v = 2 WHERE id = 1");
                read.read(&a.engine).unwrap();
                let original = participant(&a);
                b.sql("UPDATE left_t SET v = 3 WHERE id = 1");
                a.sql("ROLLBACK TO before_read");
                assert_eq!(
                    a.sql("SELECT v FROM left_t").rows[0]["v"],
                    Value::Int(2),
                    "{isolation}: {read:?} did not retain the first query's snapshot"
                );
                assert_eq!(
                    original.is_some(),
                    isolation == "SERIALIZABLE",
                    "{isolation}: {read:?} did not use the expected admission"
                );
                assert_eq!(participant(&a), original);
                a.sql("COMMIT");
                assert_eq!(a.sql("SELECT v FROM left_t").rows[0]["v"], Value::Int(3));
            }
        }
    }
}

#[test]
fn metadata_query_failures_abort_only_the_active_savepoint() {
    let (_directory, sessions) = catalog_fixtures();
    for a in sessions {
        let b = a.sibling();
        a.sql("BEGIN; UPDATE right_t SET v = 2; SAVEPOINT before_read; UPDATE right_t SET v = 3");
        a.engine.table_columns("missing_t").unwrap_err();
        assert!(a.engine.transaction_failed());
        let expected = a.engine.sql("SELECT 1", &[]).unwrap_err();
        assert_eq!(expected.sqlstate(), Some("25P02"));
        for read in CatalogRead::ALL {
            let error = read.read(&a.engine).unwrap_err();
            error.assert_transaction_error(&expected);
        }
        b.sql("UPDATE left_t SET v = 4");
        a.sql("ROLLBACK TO before_read");
        for read in CatalogRead::ALL {
            read.read(&a.engine).unwrap();
        }
        a.sql("COMMIT");
        assert_eq!(a.sql("SELECT v FROM right_t").rows[0]["v"], Value::Int(2));
        assert_eq!(a.sql("SELECT v FROM left_t").rows[0]["v"], Value::Int(4));
        a.engine.table_columns("missing_t").unwrap_err();
        assert_eq!(a.engine.transaction_depth(), 0);
        assert!(!a.engine.transaction_failed());
    }
}

#[test]
fn catalog_queries_do_not_manufacture_user_row_dependencies() {
    for read in CatalogRead::ALL {
        let (_directory, sessions) = catalog_fixtures();
        for a in sessions {
            let b = a.sibling();
            a.sql("BEGIN ISOLATION LEVEL SERIALIZABLE");
            read.read(&a.engine).unwrap();
            b.sql("BEGIN ISOLATION LEVEL SERIALIZABLE; SELECT v FROM right_t; UPDATE left_t SET v = 2; COMMIT");
            a.sql("UPDATE right_t SET v = 2; COMMIT");
            assert_eq!(a.sql("SELECT v FROM left_t").rows[0]["v"], Value::Int(2));
            assert_eq!(a.sql("SELECT v FROM right_t").rows[0]["v"], Value::Int(2));
        }
    }
}

fn deferrable_metadata_query(cancel: bool) {
    for read in [
        CatalogRead::Names,
        CatalogRead::Schemas,
        CatalogRead::Views,
        CatalogRead::Analyzers,
    ] {
        for provider in 0..3 {
            let directory = tempfile::tempdir().unwrap();
            let (writer, records) =
                observed_fixture(provider, &directory.path().join("metadata.redb"));
            let reader = writer.sibling();
            reader.sql("SET default_transaction_isolation = 'serializable'; SET default_transaction_read_only = on; SET default_transaction_deferrable = on");
            writer.sql("BEGIN ISOLATION LEVEL SERIALIZABLE; SELECT v FROM left_t");
            let previous = participant(&writer).unwrap();
            let candidate = SerializableTransactionId::new(
                previous.database(),
                previous.coordinator(),
                previous.allocation() + 1,
            )
            .unwrap();
            let cancellation = reader.engine.cancellation_token();
            std::thread::scope(|scope| {
                let query = scope.spawn(|| read.read(&reader.engine));
                pending_snapshot(records.as_ref(), candidate, &cancellation);
                if cancel {
                    cancellation.cancel();
                    let error = query.join().unwrap().unwrap_err();
                    error.assert_transaction_error(&SQLError::from(
                        cancellation.check().unwrap_err(),
                    ));
                    writer.sql("ROLLBACK");
                } else {
                    let publication = writer.engine.sql("UPDATE left_t SET v = 2; COMMIT", &[]);
                    if publication.is_err() {
                        cancellation.cancel();
                    }
                    publication.unwrap();
                    query.join().unwrap().unwrap();
                }
            });
            assert_eq!(reader.engine.transaction_depth(), 0);
            assert!(participant(&reader).is_none());
            cancellation.reset();
            read.read(&reader.engine).unwrap();
        }
    }
}

#[test]
fn implicit_metadata_queries_honor_default_deferrable_admission() {
    deferrable_metadata_query(false);
}

#[test]
fn cancelled_metadata_admission_releases_its_implicit_transaction() {
    deferrable_metadata_query(true);
}

#[test]
fn metadata_queries_in_callbacks_keep_the_outer_statement_snapshot() {
    for read in CatalogRead::ALL {
        let (_directory, sessions) = catalog_fixtures();
        for a in sessions {
            let peer = a.sibling();
            let id = a.engine.table_doc_ids("left_t").unwrap()[0];
            let engine = Arc::new(a.engine);
            let source = Arc::downgrade(&engine);
            engine
                .register_scalar_function_with_options(
                    "metadata_during_publication",
                    SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
                    move |_: &[Value]| {
                        peer.sql("UPDATE left_t SET v = 2");
                        let source = source.upgrade().unwrap();
                        read.read(&source).unwrap();
                        Ok(source.get_document("left_t", id).unwrap().unwrap()["v"].clone())
                    },
                )
                .unwrap();
            let result = engine
                .sql(
                    "SELECT v, metadata_during_publication() AS observed FROM left_t",
                    &[],
                )
                .unwrap();
            assert_eq!(result.rows[0]["v"], Value::Int(1), "{read:?}");
            assert_eq!(result.rows[0]["observed"], Value::Int(1), "{read:?}");
            assert_eq!(engine.transaction_depth(), 0);
        }
    }
}

#[test]
fn named_catalog_lookup_errors_recover_the_active_savepoint() {
    let (_directory, sessions) = catalog_fixtures();
    for a in sessions {
        let b = a.sibling();
        for invalid in [
            "index",
            "view",
            "analyzer",
            "foreign",
            "constraint",
            "model",
        ] {
            let query = |engine: &Engine| -> Result<(), CatalogError> {
                match invalid {
                    "index" => engine
                        .catalog_index("bad.name.extra")
                        .map(|_| ())
                        .map_err(Into::into),
                    "view" => engine
                        .view("bad.name.extra")
                        .map(|_| ())
                        .map_err(Into::into),
                    "analyzer" => engine
                        .get_table_analyzer("left_t", "v", "invalid")
                        .map(|_| ())
                        .map_err(Into::into),
                    "foreign" => engine
                        .foreign_table_columns("missing_foreign")
                        .map(|_| ())
                        .map_err(Into::into),
                    "constraint" => engine
                        .try_check_constraint_definitions("missing_t")
                        .map(|_| ())
                        .map_err(Into::into),
                    "model" => ignore_value(engine.deep_predict_features("missing_model", &[])),
                    _ => unreachable!(),
                }
            };
            a.sql(
                "BEGIN; UPDATE right_t SET v = 2; SAVEPOINT before_read; UPDATE right_t SET v = 3",
            );
            let error = query(&a.engine).unwrap_err();
            assert!(a.engine.transaction_failed(), "{invalid}: {error:?}");
            let expected = a.engine.sql("SELECT 1", &[]).unwrap_err();
            assert_eq!(expected.sqlstate(), Some("25P02"));
            for read in [
                CatalogRead::Schemas,
                CatalogRead::Views,
                CatalogRead::Analyzers,
            ] {
                read.read(&a.engine)
                    .unwrap_err()
                    .assert_transaction_error(&expected);
            }
            b.sql("UPDATE left_t SET v = 4");
            a.sql("ROLLBACK TO before_read; COMMIT");
            assert_eq!(a.sql("SELECT v FROM right_t").rows[0]["v"], Value::Int(2));
            assert_eq!(a.sql("SELECT v FROM left_t").rows[0]["v"], Value::Int(4));
            query(&a.engine).unwrap_err();
            assert_eq!(a.engine.transaction_depth(), 0);
            assert!(!a.engine.transaction_failed());
        }
    }
}
