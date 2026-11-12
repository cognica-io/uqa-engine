//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Untagged current and legacy foreign schemas borrow original field text before native decoding.

use super::{
    fields, invalid, json_error, retained, sql_error, take, Budgeted, ProductionControl,
    StorageBackendResult,
};
use crate::catalog::foreign::{ForeignServerReference, PersistedForeignTableSchema};
use uqa_core::{
    json::{JsonReader, JsonToken},
    CancellationToken,
};
use uqa_storage::read_control::StorageReadControl;

pub(super) fn read(
    text: &str,
    control: &StorageReadControl,
    invoking: &CancellationToken,
) -> StorageBackendResult<Budgeted<PersistedForeignTableSchema>> {
    let production = ProductionControl::new(control.memory(), control.cancellation(), invoking);
    // The original untagged decoder validates all generic content before trying either typed variant. Arbitrary-precision numeric text stays borrowed; no normalized JSON tree is needed.
    let mut reader = JsonReader::with_control(text, &production).with_depth_limit(127);
    let first = reader
        .next_event()
        .map_err(json_error)?
        .ok_or_else(invalid)?
        .token;
    let second = reader
        .next_event()
        .map_err(json_error)?
        .map(|event| event.token);
    while reader.next_event().map_err(json_error)?.is_some() {}
    if first == JsonToken::StartArray
        && matches!(
            second,
            Some(JsonToken::StartArray | JsonToken::StartObject | JsonToken::EndArray)
        )
    {
        let columns = uqa_sql::ast::read_stored_columns_content_json(
            text,
            128,
            control.memory(),
            control.cancellation(),
            invoking,
        )
        .map_err(sql_error)?;
        return retained(columns)
            .try_map(|columns| Ok(PersistedForeignTableSchema::legacy(columns)));
    }
    current(text, control, invoking)
}

fn current(
    text: &str,
    control: &StorageReadControl,
    invoking: &CancellationToken,
) -> StorageBackendResult<Budgeted<PersistedForeignTableSchema>> {
    let production = ProductionControl::new(control.memory(), control.cancellation(), invoking);
    let fields = fields::Reader::new(text, production).fields(
        [
            "version",
            "object_id",
            "catalog_oids",
            "row_type_array_name",
            "server_reference",
            "option_order",
            "columns",
            "dropped_attributes",
            "checks",
        ],
        &[0, 6],
        false,
    )?;
    let mut memory = control.memory().empty_reservation();
    let mut schema = PersistedForeignTableSchema::legacy(Vec::new());
    schema.version = fields::integer(fields[0].expect("required version"), production)?;
    if let Some(value) = fields[1] {
        schema.object_id = fields::identity(value, production)?;
    }
    if let Some(value) = present(fields[2]) {
        schema.catalog_oids = Some(
            *uqa_sql::ast::read_stored_relation_oids_content_json(
                value,
                127,
                control.memory(),
                control.cancellation(),
                invoking,
            )
            .map_err(sql_error)?,
        );
    }
    if let Some(value) = present(fields[3]) {
        schema.row_type_array_name = Some(take(fields::text(value, production)?, &mut memory));
    }
    if let Some(value) = present(fields[4]) {
        schema.server_reference = Some(reference(value, production)?);
    }
    if let Some(value) = present(fields[5]) {
        schema.option_order = Some(take(fields::texts(value, production)?, &mut memory));
    }
    schema.columns = take(
        retained(
            uqa_sql::ast::read_stored_columns_content_json(
                fields[6].expect("required columns"),
                127,
                control.memory(),
                control.cancellation(),
                invoking,
            )
            .map_err(sql_error)?,
        ),
        &mut memory,
    );
    if let Some(value) = fields[7] {
        schema.dropped_attributes = take(
            retained(
                uqa_sql::ast::read_stored_dropped_attributes_content_json(
                    value,
                    127,
                    control.memory(),
                    control.cancellation(),
                    invoking,
                )
                .map_err(sql_error)?,
            ),
            &mut memory,
        );
    }
    if let Some(value) = fields[8] {
        schema.checks = take(
            retained(
                uqa_sql::ast::read_stored_checks_content_json(
                    value,
                    127,
                    control.memory(),
                    control.cancellation(),
                    invoking,
                )
                .map_err(sql_error)?,
            ),
            &mut memory,
        );
    }
    production.check()?;
    Ok(Budgeted::new(schema, memory))
}

fn present(text: Option<&str>) -> Option<&str> {
    text.filter(|text| *text != "null")
}

fn reference(
    text: &str,
    control: ProductionControl<'_>,
) -> StorageBackendResult<ForeignServerReference> {
    let [oid, identity] =
        fields::Reader::new(text, control).fields(["oid", "object_id"], &[0, 1], true)?;
    Ok(ForeignServerReference {
        oid: fields::integer(oid.expect("required server OID"), control)?,
        object_id: fields::identity(identity.expect("required server identity"), control)?,
    })
}
