//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stream exact command keys while preserving frame masking and SQL comparison order.

use super::{
    comparison, keys, resource_error, spilled, storage_error, CommandMutationOverlay, DocId,
    KeyKind, SQLError, StorageReadControl, Value,
};

pub(super) struct MatchKeys<'a> {
    pub fields: &'a [String],
    pub values: &'a [Value],
    pub kind: KeyKind,
}

pub(super) fn visit_matches(
    overlays: &mut [CommandMutationOverlay],
    table: &str,
    keys: MatchKeys<'_>,
    control: &StorageReadControl,
    catalog: Option<&dyn uqa_sql::expr::SQLValueCatalog>,
    visit: &mut dyn FnMut(DocId) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    let MatchKeys {
        fields,
        values,
        kind,
    } = keys;
    if fields.len() != values.len() {
        return Err(SQLError::Internal(
            "command-overlay exact lookup has mismatched fields and values".into(),
        ));
    }
    control.check().map_err(resource_error)?;
    if overlays.iter().all(|overlay| !overlay.holds(table)) {
        return Ok(());
    }
    if values.iter().any(uqa_sql::expr::value_comparison_can_fail)
        || overlays.iter().any(|overlay| {
            overlay
                .table(table)
                .is_some_and(|table| table.has_fallible_comparison)
        })
    {
        return comparison::visit_matches(overlays, table, &keys, control, catalog, visit);
    }
    let (fields, key) = keys::lookup_parts(fields, values, control)?;
    for overlay in overlays.iter_mut() {
        if let Some(rows) = overlay.bind(control)?.get_mut(table) {
            rows.prepare_index(kind, &fields, control)?;
        }
    }
    for (position, overlay) in overlays.iter().enumerate().rev() {
        let Some(rows) = overlay.table(table) else {
            continue;
        };
        let newer = &overlays[position + 1..];
        let index = rows
            .indexes(kind)
            .get(fields.values())
            .expect("prepared exact index");
        for (&id, ()) in index.candidates(key.bytes()).into_iter().flatten() {
            control.check().map_err(resource_error)?;
            if !CommandMutationOverlay::stages(newer, table, id, control)? {
                visit(id)?;
            }
        }
        let Some(spilled) = &rows.rows.spilled else {
            continue;
        };
        let mut after = None;
        loop {
            let page =
                spilled::index_page(spilled.view(), index.ordinal(), key.bytes(), after, control)
                    .map_err(storage_error)?;
            for (id, _) in page.rows.iter().copied() {
                control.check().map_err(resource_error)?;
                if rows.rows.memory.get(&id).is_none()
                    && !CommandMutationOverlay::stages(newer, table, id, control)?
                {
                    visit(id)?;
                }
            }
            match page.resume {
                Some(resume) => after = Some(resume),
                None => break,
            }
        }
    }
    Ok(())
}
