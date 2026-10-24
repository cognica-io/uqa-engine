//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use arrow_array::{Array, Decimal128Array, Int16Array, Int32Array, LargeStringArray};
use arrow_schema::DataType;
use uqa_api::arrow_export::{export_arrow, ArrowExportPolicy};
use uqa_core::memory::{MemoryBudget, ProductionControl};
use uqa_engine::Engine;

#[test]
fn declared_widths_duplicate_labels_and_empty_descriptors_survive() {
    let engine = Engine::new();
    for predicate in ["TRUE", "FALSE"] {
        let result = engine.sql(&format!(
            "SELECT 7::smallint AS x, 8::integer AS x, NULL::bigint AS nil, 'a  '::char(3) AS padded, 1.25::real AS r WHERE {predicate}"
        ), &[]).unwrap();
        let batch = export_arrow(
            &result,
            ArrowExportPolicy::Lossless,
            Some(&engine),
            &ProductionControl::uncontrolled(),
        )
        .unwrap();
        assert_eq!(batch.num_columns(), 5);
        assert_eq!(batch.schema().field(0).name(), "x");
        assert_eq!(batch.schema().field(1).name(), "x");
        assert_eq!(batch.schema().field(0).data_type(), &DataType::Int16);
        assert_eq!(batch.schema().field(1).data_type(), &DataType::Int32);
        assert_eq!(batch.schema().field(2).data_type(), &DataType::Int64);
        assert_eq!(batch.schema().field(4).data_type(), &DataType::Float32);
        assert_eq!(batch.schema().field(0).metadata()["uqa.sql.type_oid"], "21");
        if predicate == "TRUE" {
            assert_eq!(
                batch
                    .column(0)
                    .as_any()
                    .downcast_ref::<Int16Array>()
                    .unwrap()
                    .value(0),
                7
            );
            assert_eq!(
                batch
                    .column(1)
                    .as_any()
                    .downcast_ref::<Int32Array>()
                    .unwrap()
                    .value(0),
                8
            );
            assert!(batch.column(2).is_null(0));
            assert_eq!(
                batch
                    .column(3)
                    .as_any()
                    .downcast_ref::<LargeStringArray>()
                    .unwrap()
                    .value(0),
                "a  "
            );
        } else {
            assert_eq!(batch.num_rows(), 0);
        }
    }
}

#[test]
fn numeric_is_exact_and_unqualified_values_require_explicit_text() {
    let engine = Engine::new();
    let result = engine.sql("SELECT 12345678901234567890.125::numeric(25,3) AS exact, 1200::numeric(3,-2) AS negative", &[]).unwrap();
    let batch = export_arrow(
        &result,
        ArrowExportPolicy::Lossless,
        Some(&engine),
        &ProductionControl::uncontrolled(),
    )
    .unwrap();
    assert_eq!(
        batch
            .column(0)
            .as_any()
            .downcast_ref::<Decimal128Array>()
            .unwrap()
            .value(0),
        12_345_678_901_234_567_890_125_i128
    );
    assert_eq!(
        batch
            .column(1)
            .as_any()
            .downcast_ref::<Decimal128Array>()
            .unwrap()
            .value(0),
        12
    );
    for sql in [
        "SELECT 1.25::numeric AS n",
        "SELECT 'NaN'::numeric(10,2) AS n",
        "SELECT 'infinity'::timestamp AS t",
        "SELECT '[0:1]={1,2}'::integer[] AS a",
        "SELECT 0.01::numeric(2,3) AS n",
    ] {
        let result = engine.sql(sql, &[]).unwrap();
        let error = export_arrow(
            &result,
            ArrowExportPolicy::Lossless,
            Some(&engine),
            &ProductionControl::uncontrolled(),
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("0A000"), "{sql}: {error}");
        let batch = export_arrow(
            &result,
            ArrowExportPolicy::SQLText,
            Some(&engine),
            &ProductionControl::uncontrolled(),
        )
        .unwrap();
        assert_eq!(batch.schema().field(0).data_type(), &DataType::LargeUtf8);
        assert!(!batch.column(0).is_null(0));
    }
}

#[test]
fn json_uuid_bytes_and_domains_keep_their_declared_identity() {
    let engine = Engine::new();
    let result = engine.sql("SELECT '{\"k\":1, \"k\":2}'::json AS j, '{\"k\":1, \"k\":2}'::jsonb AS b, '00112233-4455-6677-8899-aabbccddeeff'::uuid AS u, '\\x0001ff'::bytea AS bytes", &[]).unwrap();
    let batch = export_arrow(
        &result,
        ArrowExportPolicy::Lossless,
        Some(&engine),
        &ProductionControl::uncontrolled(),
    )
    .unwrap();
    assert_eq!(
        batch.schema().field(0).metadata()["uqa.sql.type_name"],
        "json"
    );
    assert_eq!(
        batch.schema().field(1).metadata()["uqa.sql.type_name"],
        "jsonb"
    );
    assert_eq!(
        batch.schema().field(2).data_type(),
        &DataType::FixedSizeBinary(16)
    );
    assert_eq!(batch.schema().field(3).data_type(), &DataType::LargeBinary);
    assert_eq!(
        batch
            .column(0)
            .as_any()
            .downcast_ref::<LargeStringArray>()
            .unwrap()
            .value(0),
        "{\"k\":1, \"k\":2}"
    );
    engine
        .sql(
            "CREATE DOMAIN arrow_domain AS integer CHECK (VALUE > 0)",
            &[],
        )
        .unwrap();
    let result = engine.sql("SELECT 5::arrow_domain AS d", &[]).unwrap();
    assert!(export_arrow(
        &result,
        ArrowExportPolicy::Lossless,
        Some(&engine),
        &ProductionControl::uncontrolled()
    )
    .is_err());
    let batch = export_arrow(
        &result,
        ArrowExportPolicy::SQLText,
        Some(&engine),
        &ProductionControl::uncontrolled(),
    )
    .unwrap();
    assert_eq!(
        batch
            .column(0)
            .as_any()
            .downcast_ref::<LargeStringArray>()
            .unwrap()
            .value(0),
        "5"
    );
    assert_eq!(
        batch.schema().field(0).metadata()["uqa.sql.type_schema"],
        "public"
    );
}

#[test]
fn export_admission_and_cancellation_release_partial_output() {
    let engine = Engine::new();
    let result = engine
        .sql("SELECT repeat('x', 10000) AS payload, 7 AS n", &[])
        .unwrap();
    let original = uqa_core::CancellationToken::new();
    let invoking = uqa_core::CancellationToken::new();
    let budget = MemoryBudget::new(1024);
    let control = ProductionControl::new(&budget, &original, &invoking);
    assert!(export_arrow(
        &result,
        ArrowExportPolicy::Lossless,
        Some(&engine),
        &control
    )
    .is_err());
    assert_eq!(budget.used(), 0);
    let budget = MemoryBudget::new(100_000);
    let control = ProductionControl::new(&budget, &original, &invoking);
    let batch = export_arrow(
        &result,
        ArrowExportPolicy::Lossless,
        Some(&engine),
        &control,
    )
    .unwrap();
    let retained = budget.used();
    assert!(retained >= batch.get_array_memory_size());
    assert_eq!(batch.num_rows(), 1);
    invoking.cancel();
    assert_eq!(
        export_arrow(
            &result,
            ArrowExportPolicy::Lossless,
            Some(&engine),
            &control
        )
        .unwrap_err()
        .sqlstate(),
        Some("57014")
    );
    assert_eq!(
        batch
            .column(0)
            .as_any()
            .downcast_ref::<LargeStringArray>()
            .unwrap()
            .value(0)
            .len(),
        10000
    );
    assert_eq!(budget.used(), retained);
    drop(batch);
    assert_eq!(budget.used(), 0);
}
