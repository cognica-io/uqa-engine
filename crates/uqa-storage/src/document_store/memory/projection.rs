//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Layout-compiled borrowed field projection.

use super::{DocId, Document, MemoryDocumentStore, Value};

impl MemoryDocumentStore {
    pub(super) fn visit_fields_multi_ref_with_presence(
        &self,
        doc_ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
    ) {
        let null = Value::Null;
        let mut values = Vec::with_capacity(fields.len());
        let fields_are_sorted = fields.windows(2).all(|pair| pair[0] <= pair[1]);
        let layout_projections = self
            .layouts
            .iter()
            .map(|layout| ProjectedLayout::compile(layout, fields))
            .collect::<Vec<_>>();

        if should_merge_projected_scan(doc_ids, self.documents.len()) {
            self.visit_merge_scan(
                doc_ids,
                fields,
                fields_are_sorted,
                &layout_projections,
                &null,
                &mut values,
                visitor,
            );
            return;
        }

        for doc_id in doc_ids {
            let document = self.documents.get(doc_id);
            let projection = self
                .document_layout_ids
                .get(doc_id)
                .and_then(|layout_id| layout_projections.get(*layout_id));
            project_document(
                document,
                projection,
                fields,
                fields_are_sorted,
                &null,
                &mut values,
            );
            if !visitor(*doc_id, document.is_some(), &values) {
                return;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn visit_merge_scan<'a>(
        &'a self,
        doc_ids: &[DocId],
        fields: &[&str],
        fields_are_sorted: bool,
        layout_projections: &[ProjectedLayout],
        null: &'a Value,
        values: &mut Vec<&'a Value>,
        visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
    ) {
        let mut documents = self.documents.range(doc_ids[0]..);
        let mut current = documents.next();
        let mut layout_ids = self.document_layout_ids.range(doc_ids[0]..);
        let mut current_layout = layout_ids.next();
        for doc_id in doc_ids {
            while current.is_some_and(|(stored_id, _)| stored_id < doc_id) {
                current = documents.next();
            }
            while current_layout.is_some_and(|(stored_id, _)| stored_id < doc_id) {
                current_layout = layout_ids.next();
            }
            let document = match current {
                Some((stored_id, document)) if stored_id == doc_id => Some(document),
                _ => None,
            };
            let projection = match current_layout {
                Some((stored_id, layout_id)) if stored_id == doc_id => {
                    layout_projections.get(*layout_id)
                }
                _ => None,
            };
            project_document(
                document,
                projection,
                fields,
                fields_are_sorted,
                null,
                values,
            );
            if !visitor(*doc_id, document.is_some(), values) {
                return;
            }
        }
    }
}

pub(super) fn should_merge_projected_scan(doc_ids: &[DocId], document_count: usize) -> bool {
    let Some((&first, &last)) = doc_ids.first().zip(doc_ids.last()) else {
        return false;
    };
    if doc_ids.len() < 2 || !doc_ids.windows(2).all(|pair| pair[0] <= pair[1]) {
        return false;
    }
    let probe_budget = (doc_ids.len() as u128).saturating_mul(8);
    let id_span = u128::from(last)
        .saturating_sub(u128::from(first))
        .saturating_add(1);
    id_span <= probe_budget || probe_budget >= document_count as u128
}

struct ProjectedLayout {
    requested_len: usize,
    bindings: Vec<(usize, usize)>,
}

impl ProjectedLayout {
    fn compile(layout: &[String], fields: &[&str]) -> Self {
        let mut bindings = fields
            .iter()
            .enumerate()
            .filter_map(|(output_slot, field)| {
                layout
                    .binary_search_by(|stored| stored.as_str().cmp(field))
                    .ok()
                    .map(|stored_slot| (stored_slot, output_slot))
            })
            .collect::<Vec<_>>();
        bindings.sort_unstable();
        Self {
            requested_len: fields.len(),
            bindings,
        }
    }

    fn project<'a>(
        &self,
        document: Option<&'a Document>,
        null: &'a Value,
        values: &mut Vec<&'a Value>,
    ) {
        values.clear();
        values.resize(self.requested_len, null);
        let Some(document) = document else {
            return;
        };
        let mut next_binding = 0;
        for (stored_slot, value) in document.values().enumerate() {
            while self
                .bindings
                .get(next_binding)
                .is_some_and(|(bound_slot, _)| *bound_slot == stored_slot)
            {
                values[self.bindings[next_binding].1] = value;
                next_binding += 1;
            }
            if next_binding == self.bindings.len() {
                break;
            }
        }
    }
}

fn project_document<'a>(
    document: Option<&'a Document>,
    projection: Option<&ProjectedLayout>,
    fields: &[&str],
    fields_are_sorted: bool,
    null: &'a Value,
    values: &mut Vec<&'a Value>,
) {
    if let Some(projection) = projection {
        projection.project(document, null, values);
    } else {
        project_document_fields_ref(document, fields, fields_are_sorted, null, values);
    }
}

fn project_document_fields_ref<'a>(
    document: Option<&'a Document>,
    fields: &[&str],
    fields_are_sorted: bool,
    null: &'a Value,
    values: &mut Vec<&'a Value>,
) {
    values.clear();
    match document {
        Some(document) if fields_are_sorted => {
            let mut stored_fields = document.iter();
            let mut current = stored_fields.next();
            for requested in fields {
                while current.is_some_and(|(stored, _)| stored.as_str() < *requested) {
                    current = stored_fields.next();
                }
                values.push(match current {
                    Some((stored, value)) if stored.as_str() == *requested => value,
                    _ => null,
                });
            }
        }
        Some(document) => values.extend(
            fields
                .iter()
                .map(|field| document.get(*field).unwrap_or(null)),
        ),
        None => values.resize(fields.len(), null),
    }
}
