//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Apache AGE catalog synthesis: `ag_catalog.ag_graph`,
//! `ag_catalog.ag_label`, and the namespace, relation, attribute,
//! sequence, and type rows that mirror every named graph into the
//! `PostgreSQL` catalogs the way the extension does.
//!
//! AGE keeps one schema per graph. The schema holds the default
//! `_ag_label_vertex` / `_ag_label_edge` relations, one relation per user
//! label, the `_label_id_seq` sequence, and one `<label>_id_seq` sequence
//! per label. The engine stores graphs natively, so these rows are derived
//! from the graph registry instead of physical tables.

use uqa_core::Value;
use uqa_graph::{GraphLabelInfo, GraphStore as _, LabelKind};
use uqa_sql::ast::ColumnType;
use uqa_sql::expr::quote_ident;
use uqa_sql::{ResultRow, SQLError};

use super::helpers::{
    bool_value, catalog_name, current_user_name, current_user_oid, int_value, relation_oid, row,
    schema_oid, stable_oid, str_value,
};
use super::pg_catalog::pg_class_row;
use super::schema::{ag_catalog_type_oid, age_agtype, age_graphid, AG_CATALOG_SCHEMA};
use crate::{Engine, RelationIdentity};

/// AGE `_label_id_seq`: the per-graph label id allocator.
const LABEL_ID_SEQUENCE: &str = "_label_id_seq";
/// AGE label id domain bound (`label_id` is `int` in `1..=65535`).
const MAX_LABEL_ID: i64 = 65_535;
/// AGE per-label sequences run over the 48-bit entry id space.
const MAX_ENTRY_ID: i64 = (1_i64 << 48) - 1;

/// One graph with its `ag_label` entries.
pub(super) struct GraphCatalogEntry {
    pub(super) name: String,
    pub(super) labels: Vec<GraphLabelInfo>,
}

#[derive(Clone)]
pub(super) struct AgeLabelRelation {
    graph: String,
    label: GraphLabelInfo,
}

impl AgeLabelRelation {
    fn canonical_name(&self) -> String {
        label_relation_regclass(&self.graph, &self.label.name)
    }
}

pub(super) fn graph_catalog_entries(engine: &Engine) -> Result<Vec<GraphCatalogEntry>, SQLError> {
    Ok(engine
        .graph_label_catalog()
        .map_err(|err| SQLError::Internal(format!("read graph catalog: {err}")))?
        .into_iter()
        .map(|(name, labels)| GraphCatalogEntry { name, labels })
        .collect())
}

/// Resolve a graph-label relation through an explicit graph schema or the
/// current `search_path`. Only surviving `ag_label` entries are relations;
/// dropped default and user labels remain absent across reopen.
pub(super) fn resolve_age_label_relation(
    engine: &Engine,
    name: &str,
) -> Result<Option<AgeLabelRelation>, SQLError> {
    let (schema, label_name) = RelationIdentity::parse_reference(name).map_err(|error| {
        SQLError::Internal(format!("invalid AGE label relation `{name}`: {error}"))
    })?;
    let graph_names = schema.map_or_else(|| engine.search_path(), |schema| vec![schema]);
    for graph_name in graph_names {
        let Some(labels) = engine
            .list_graph_labels(&graph_name)
            .map_err(|err| SQLError::Internal(format!("read graph labels: {err}")))?
        else {
            continue;
        };
        if let Some(label) = labels.into_iter().find(|label| label.name == label_name) {
            return Ok(Some(AgeLabelRelation {
                graph: graph_name,
                label,
            }));
        }
    }
    Ok(None)
}

pub(crate) fn resolve_age_label_relation_name(
    engine: &Engine,
    name: &str,
) -> Result<Option<String>, SQLError> {
    Ok(resolve_age_label_relation(engine, name)?.map(|relation| relation.canonical_name()))
}

pub(super) fn age_label_relation_schema(
    engine: &Engine,
    name: &str,
) -> Result<Option<Vec<(String, ColumnType)>>, SQLError> {
    let Some(relation) = resolve_age_label_relation(engine, name)? else {
        return Ok(None);
    };
    let columns = match relation.label.kind {
        LabelKind::Vertex => vec![
            ("id".into(), age_graphid()),
            ("properties".into(), age_agtype()),
        ],
        LabelKind::Edge => vec![
            ("id".into(), age_graphid()),
            ("start_id".into(), age_graphid()),
            ("end_id".into(), age_graphid()),
            ("properties".into(), age_agtype()),
        ],
    };
    Ok(Some(columns))
}

pub(super) fn build_age_label_relation_rows(
    engine: &Engine,
    name: &str,
) -> Result<Option<Vec<ResultRow>>, SQLError> {
    let Some(relation) = resolve_age_label_relation(engine, name)? else {
        return Ok(None);
    };
    let rows = engine
        .graph_with(&relation.graph, |store| match relation.label.kind {
            LabelKind::Vertex => store.vertices_in_graph(&relation.graph).map(|vertices| {
                vertices
                    .into_iter()
                    .filter(|vertex| relation.includes_graphid(vertex.vertex_id))
                    .map(|vertex| {
                        Ok(row([
                            ("id", graphid_value(vertex.vertex_id)?),
                            (
                                "properties",
                                Value::Str(uqa_graph::agtype::render(&Value::Map(
                                    vertex.properties,
                                ))),
                            ),
                        ]))
                    })
                    .collect::<Result<Vec<_>, SQLError>>()
            }),
            LabelKind::Edge => store.edges_in_graph(&relation.graph).map(|edges| {
                edges
                    .into_iter()
                    .filter(|edge| relation.includes_graphid(edge.edge_id))
                    .map(|edge| {
                        Ok(row([
                            ("id", graphid_value(edge.edge_id)?),
                            ("start_id", graphid_value(edge.source_id)?),
                            ("end_id", graphid_value(edge.target_id)?),
                            (
                                "properties",
                                Value::Str(uqa_graph::agtype::render(&Value::Map(edge.properties))),
                            ),
                        ]))
                    })
                    .collect::<Result<Vec<_>, SQLError>>()
            }),
        })
        .map_err(|error| SQLError::Internal(format!("read AGE label relation `{name}`: {error}")))?
        .ok_or_else(|| {
            SQLError::Internal(format!(
                "AGE label relation `{name}` resolved after graph `{}` disappeared",
                relation.graph
            ))
        })?
        .map_err(|error| {
            SQLError::Internal(format!("scan AGE label relation `{name}`: {error}"))
        })??;
    Ok(Some(rows))
}

impl AgeLabelRelation {
    fn includes_graphid(&self, graphid: u64) -> bool {
        self.label.id == self.label.kind.default_label_id()
            || uqa_graph::graphid_label_id(graphid) == self.label.id
    }
}

fn graphid_value(graphid: u64) -> Result<Value, SQLError> {
    i64::try_from(graphid).map(Value::Int).map_err(|_| {
        SQLError::Internal(format!(
            "AGE graphid {graphid} exceeds the signed 64-bit graphid range"
        ))
    })
}

/// `ag_graph.graphid` of a named graph.
pub(super) fn graph_oid(graph: &str) -> i64 {
    stable_oid("graph", graph)
}

/// `pg_class.oid` of a label relation.
pub(super) fn label_relation_oid(graph: &str, label: &str) -> i64 {
    relation_oid("r", graph, label)
}

/// `regclass` text of a label relation, quoted like `PostgreSQL` prints
/// relation names outside the search path.
pub(super) fn label_relation_regclass(graph: &str, label: &str) -> String {
    format!("{}.{}", quote_ident(graph), quote_ident(label))
}

/// `ag_label.seq_name` of a label.
pub(super) fn label_sequence_name(label: &str) -> String {
    format!("{label}_id_seq")
}

/// Every namespace AGE adds to the database: `ag_catalog` plus one
/// schema per graph.
pub(super) fn age_namespace_names(engine: &Engine) -> Result<Vec<String>, SQLError> {
    let mut names = vec![AG_CATALOG_SCHEMA.to_string()];
    names.extend(
        engine
            .list_graphs()
            .map_err(|err| SQLError::Internal(format!("read graph catalog: {err}")))?,
    );
    Ok(names)
}

pub(super) fn build_ag_graph(engine: &Engine) -> Result<Vec<ResultRow>, SQLError> {
    Ok(graph_catalog_entries(engine)?
        .into_iter()
        .map(|entry| {
            row([
                ("graphid", int_value(graph_oid(&entry.name))),
                ("name", str_value(entry.name.clone())),
                ("namespace", str_value(entry.name)),
            ])
        })
        .collect())
}

pub(super) fn build_ag_label(engine: &Engine) -> Result<Vec<ResultRow>, SQLError> {
    let mut out = Vec::new();
    for entry in graph_catalog_entries(engine)? {
        for label in &entry.labels {
            out.push(row([
                ("name", str_value(label.name.clone())),
                ("graph", int_value(graph_oid(&entry.name))),
                ("id", int_value(i64::from(label.id))),
                ("kind", str_value(label.kind.as_char().to_string())),
                (
                    "relation",
                    str_value(label_relation_regclass(&entry.name, &label.name)),
                ),
                ("seq_name", str_value(label_sequence_name(&label.name))),
            ]));
        }
    }
    Ok(out)
}

/// `pg_class.reltuples` of a label relation. The default label relations
/// physically hold only the unlabeled entities; user label relations hold
/// the entities that carry their label.
fn label_relation_tuples(engine: &Engine, graph: &str, label: &GraphLabelInfo) -> f64 {
    let stored_label = if label.id == uqa_graph::VERTEX_DEFAULT_LABEL_ID
        || label.id == uqa_graph::EDGE_DEFAULT_LABEL_ID
    {
        ""
    } else {
        label.name.as_str()
    };
    let count = engine
        .graph_with(graph, |store| {
            use uqa_graph::GraphStore as _;
            match label.kind {
                LabelKind::Vertex => store
                    .vertex_ids_by_label(stored_label, graph)
                    .map(|ids| ids.len())
                    .unwrap_or(0),
                LabelKind::Edge => store
                    .edge_ids_by_label(stored_label, graph)
                    .map(|ids| ids.len())
                    .unwrap_or(0),
            }
        })
        .ok()
        .flatten()
        .unwrap_or(0);
    count as f64
}

/// AGE label relations expose `id graphid, properties agtype` for
/// vertices and `id graphid, start_id graphid, end_id graphid,
/// properties agtype` for edges.
fn label_columns(kind: LabelKind) -> &'static [(&'static str, &'static str)] {
    match kind {
        LabelKind::Vertex => &[("id", "graphid"), ("properties", "agtype")],
        LabelKind::Edge => &[
            ("id", "graphid"),
            ("start_id", "graphid"),
            ("end_id", "graphid"),
            ("properties", "agtype"),
        ],
    }
}

/// `pg_class` rows for every label relation and label sequence of every
/// graph.
pub(super) fn age_pg_class_rows(engine: &Engine) -> Result<Vec<ResultRow>, SQLError> {
    let mut out = Vec::new();
    for entry in graph_catalog_entries(engine)? {
        out.push(pg_class_row(
            &entry.name,
            LABEL_ID_SEQUENCE,
            "S",
            0,
            0.0,
            false,
        ));
        for label in &entry.labels {
            let natts = i64::try_from(label_columns(label.kind).len())
                .map_err(|_| SQLError::Internal("label column count".into()))?;
            out.push(pg_class_row(
                &entry.name,
                &label.name,
                "r",
                natts,
                label_relation_tuples(engine, &entry.name, label),
                false,
            ));
            out.push(pg_class_row(
                &entry.name,
                &label_sequence_name(&label.name),
                "S",
                0,
                0.0,
                false,
            ));
        }
    }
    Ok(out)
}

/// `pg_attribute` rows for the columns of every label relation.
pub(super) fn age_pg_attribute_rows(engine: &Engine) -> Result<Vec<ResultRow>, SQLError> {
    let mut out = Vec::new();
    for entry in graph_catalog_entries(engine)? {
        for label in &entry.labels {
            let relid = label_relation_oid(&entry.name, &label.name);
            for (index, (column, type_name)) in label_columns(label.kind).iter().enumerate() {
                let attnum = i64::try_from(index + 1)
                    .map_err(|_| SQLError::Internal("label attribute number".into()))?;
                out.push(age_pg_attribute_row(relid, attnum, column, type_name));
            }
        }
    }
    Ok(out)
}

fn age_pg_attribute_row(relid: i64, attnum: i64, column: &str, type_name: &str) -> ResultRow {
    // `graphid` is a fixed 8-byte pass-by-value type; `agtype` is a
    // varlena defined `LIKE = jsonb`.
    let (attlen, byval, align, storage) = match type_name {
        "graphid" => (8, true, "d", "p"),
        _ => (-1, false, "i", "x"),
    };
    row([
        ("attrelid", int_value(relid)),
        ("attname", str_value(column)),
        (
            "atttypid",
            int_value(i64::from(ag_catalog_type_oid(type_name))),
        ),
        ("attstattarget", int_value(-1)),
        ("attlen", int_value(attlen)),
        ("attnum", int_value(attnum)),
        ("attndims", int_value(0)),
        ("atttypmod", int_value(-1)),
        ("attbyval", bool_value(byval)),
        ("attalign", str_value(align)),
        ("attstorage", str_value(storage)),
        ("attcompression", str_value("")),
        (
            "attnotnull",
            bool_value(matches!(column, "id" | "start_id" | "end_id")),
        ),
        ("atthasdef", bool_value(column == "id")),
        ("atthasmissing", bool_value(false)),
        ("attidentity", str_value("")),
        ("attgenerated", str_value("")),
        ("attisdropped", bool_value(false)),
        ("attislocal", bool_value(true)),
        ("attinhcount", int_value(0)),
        ("attcollation", int_value(0)),
        ("attacl", Value::Null),
        ("attoptions", Value::Null),
        ("attfdwoptions", Value::Null),
        ("attmissingval", Value::Null),
    ])
}

/// `pg_sequences` rows for `_label_id_seq` and every `<label>_id_seq`.
pub(super) fn age_pg_sequences_rows(engine: &Engine) -> Result<Vec<ResultRow>, SQLError> {
    let mut out = Vec::new();
    for entry in graph_catalog_entries(engine)? {
        let next_label_id = engine
            .graph_with(&entry.name, |store| {
                store.label_registry(&entry.name).next_label_id
            })
            .map_err(|err| SQLError::Internal(format!("read graph label catalog: {err}")))?
            .unwrap_or(uqa_graph::FIRST_USER_LABEL_ID);
        let last_label_id = i64::from(next_label_id).saturating_sub(1);
        out.push(age_pg_sequence_row(
            &entry.name,
            LABEL_ID_SEQUENCE,
            "integer",
            MAX_LABEL_ID,
            (last_label_id >= i64::from(uqa_graph::FIRST_USER_LABEL_ID)).then_some(last_label_id),
        ));
        for label in &entry.labels {
            let last_value = i64::try_from(label.last_sequence).unwrap_or(i64::MAX);
            out.push(age_pg_sequence_row(
                &entry.name,
                &label_sequence_name(&label.name),
                "bigint",
                MAX_ENTRY_ID,
                (last_value > 0).then_some(last_value),
            ));
        }
    }
    Ok(out)
}

fn age_pg_sequence_row(
    schema: &str,
    name: &str,
    data_type: &str,
    max_value: i64,
    last_value: Option<i64>,
) -> ResultRow {
    row([
        ("schemaname", str_value(schema)),
        ("sequencename", str_value(name)),
        ("sequenceowner", str_value(current_user_name())),
        ("data_type", str_value(data_type)),
        ("start_value", int_value(1)),
        ("min_value", int_value(1)),
        ("max_value", int_value(max_value)),
        ("increment_by", int_value(1)),
        ("cycle", bool_value(false)),
        ("cache_size", int_value(1)),
        ("last_value", last_value.map_or(Value::Null, int_value)),
    ])
}

/// `information_schema.tables` rows for the label relations.
pub(super) fn age_info_table_rows(engine: &Engine) -> Result<Vec<ResultRow>, SQLError> {
    let mut out = Vec::new();
    for entry in graph_catalog_entries(engine)? {
        for label in &entry.labels {
            out.push(row([
                ("table_catalog", catalog_name()),
                ("table_schema", str_value(entry.name.clone())),
                ("table_name", str_value(label.name.clone())),
                ("table_type", str_value("BASE TABLE")),
                ("self_referencing_column_name", Value::Null),
                ("reference_generation", Value::Null),
                ("user_defined_type_catalog", Value::Null),
                ("user_defined_type_schema", Value::Null),
                ("user_defined_type_name", Value::Null),
                ("is_insertable_into", str_value("YES")),
                ("is_typed", str_value("NO")),
                ("commit_action", Value::Null),
            ]));
        }
    }
    Ok(out)
}

/// `information_schema.columns` rows for the label relations.
pub(super) fn age_info_column_rows(engine: &Engine) -> Result<Vec<ResultRow>, SQLError> {
    let mut out = Vec::new();
    for entry in graph_catalog_entries(engine)? {
        for label in &entry.labels {
            for (index, (column, type_name)) in label_columns(label.kind).iter().enumerate() {
                let ordinal = i64::try_from(index + 1)
                    .map_err(|_| SQLError::Internal("label column ordinal".into()))?;
                out.push(row([
                    ("table_catalog", catalog_name()),
                    ("table_schema", str_value(entry.name.clone())),
                    ("table_name", str_value(label.name.clone())),
                    ("column_name", str_value(*column)),
                    ("ordinal_position", int_value(ordinal)),
                    (
                        "column_default",
                        if *column == "id" {
                            str_value(format!(
                                "_graphid((_label_id('{}'::name, '{}'::name))::integer, nextval('{}'::regclass))",
                                entry.name,
                                label.name,
                                label_relation_regclass(
                                    &entry.name,
                                    &label_sequence_name(&label.name)
                                )
                            ))
                        } else {
                            Value::Null
                        },
                    ),
                    (
                        "is_nullable",
                        str_value(if *column == "id" { "NO" } else { "YES" }),
                    ),
                    ("data_type", str_value("USER-DEFINED")),
                    ("character_maximum_length", Value::Null),
                    ("character_octet_length", Value::Null),
                    ("numeric_precision", Value::Null),
                    ("numeric_precision_radix", Value::Null),
                    ("numeric_scale", Value::Null),
                    ("datetime_precision", Value::Null),
                    ("interval_type", Value::Null),
                    ("interval_precision", Value::Null),
                    ("character_set_catalog", Value::Null),
                    ("character_set_schema", Value::Null),
                    ("character_set_name", Value::Null),
                    ("collation_catalog", Value::Null),
                    ("collation_schema", Value::Null),
                    ("collation_name", Value::Null),
                    ("domain_catalog", Value::Null),
                    ("domain_schema", Value::Null),
                    ("domain_name", Value::Null),
                    ("udt_catalog", catalog_name()),
                    ("udt_schema", str_value(AG_CATALOG_SCHEMA)),
                    ("udt_name", str_value(*type_name)),
                    ("scope_catalog", Value::Null),
                    ("scope_schema", Value::Null),
                    ("scope_name", Value::Null),
                    ("maximum_cardinality", Value::Null),
                    ("dtd_identifier", str_value(ordinal.to_string())),
                    ("is_self_referencing", str_value("NO")),
                    ("is_identity", str_value("NO")),
                    ("identity_generation", Value::Null),
                    ("identity_start", Value::Null),
                    ("identity_increment", Value::Null),
                    ("identity_maximum", Value::Null),
                    ("identity_minimum", Value::Null),
                    ("identity_cycle", str_value("NO")),
                    ("is_generated", str_value("NEVER")),
                    ("generation_expression", Value::Null),
                    ("is_updatable", str_value("YES")),
                ]));
            }
        }
    }
    Ok(out)
}

/// `pg_type` rows for the AGE extension types: `agtype` (defined
/// `LIKE = jsonb`), the fixed 8-byte `graphid`, and their array types.
/// The `label_id` / `label_kind` domains are emitted with the other
/// domains by the `pg_type` builder.
pub(super) fn age_pg_type_rows() -> Vec<ResultRow> {
    let namespace = schema_oid(AG_CATALOG_SCHEMA);
    let agtype = i64::from(ag_catalog_type_oid("agtype"));
    let agtype_array = i64::from(ag_catalog_type_oid("_agtype"));
    let graphid = i64::from(ag_catalog_type_oid("graphid"));
    let graphid_array = i64::from(ag_catalog_type_oid("_graphid"));
    vec![
        age_pg_type_row(AgeTypeRow {
            oid: agtype,
            name: "agtype",
            namespace,
            len: -1,
            by_value: false,
            category: "U",
            subscript: "-",
            element_oid: 0,
            array_oid: agtype_array,
            io: ("agtype_in", "agtype_out", "agtype_recv", "agtype_send"),
            align: "i",
            storage: "x",
        }),
        age_pg_type_row(AgeTypeRow {
            oid: agtype_array,
            name: "_agtype",
            namespace,
            len: -1,
            by_value: false,
            category: "A",
            subscript: "array_subscript_handler",
            element_oid: agtype,
            array_oid: 0,
            io: ("array_in", "array_out", "array_recv", "array_send"),
            align: "i",
            storage: "x",
        }),
        age_pg_type_row(AgeTypeRow {
            oid: graphid,
            name: "graphid",
            namespace,
            len: 8,
            by_value: true,
            category: "U",
            subscript: "-",
            element_oid: 0,
            array_oid: graphid_array,
            io: ("graphid_in", "graphid_out", "graphid_recv", "graphid_send"),
            align: "d",
            storage: "p",
        }),
        age_pg_type_row(AgeTypeRow {
            oid: graphid_array,
            name: "_graphid",
            namespace,
            len: -1,
            by_value: false,
            category: "A",
            subscript: "array_subscript_handler",
            element_oid: graphid,
            array_oid: 0,
            io: ("array_in", "array_out", "array_recv", "array_send"),
            align: "d",
            storage: "x",
        }),
    ]
}

struct AgeTypeRow {
    oid: i64,
    name: &'static str,
    namespace: i64,
    len: i64,
    by_value: bool,
    category: &'static str,
    subscript: &'static str,
    element_oid: i64,
    array_oid: i64,
    io: (&'static str, &'static str, &'static str, &'static str),
    align: &'static str,
    storage: &'static str,
}

fn age_pg_type_row(ty: AgeTypeRow) -> ResultRow {
    let (input, output, receive, send) = ty.io;
    row([
        ("oid", int_value(ty.oid)),
        ("typname", str_value(ty.name)),
        ("typnamespace", int_value(ty.namespace)),
        ("typowner", int_value(current_user_oid())),
        ("typlen", int_value(ty.len)),
        ("typbyval", bool_value(ty.by_value)),
        ("typtype", str_value("b")),
        ("typcategory", str_value(ty.category)),
        ("typispreferred", bool_value(false)),
        ("typisdefined", bool_value(true)),
        ("typdelim", str_value(",")),
        ("typrelid", int_value(0)),
        ("typsubscript", str_value(ty.subscript)),
        ("typelem", int_value(ty.element_oid)),
        ("typarray", int_value(ty.array_oid)),
        ("typinput", str_value(input)),
        ("typoutput", str_value(output)),
        ("typreceive", str_value(receive)),
        ("typsend", str_value(send)),
        ("typmodin", str_value("-")),
        ("typmodout", str_value("-")),
        ("typanalyze", str_value("-")),
        ("typalign", str_value(ty.align)),
        ("typstorage", str_value(ty.storage)),
        ("typnotnull", bool_value(false)),
        ("typbasetype", int_value(0)),
        ("typtypmod", int_value(-1)),
        ("typndims", int_value(0)),
        ("typcollation", int_value(0)),
        ("typdefaultbin", Value::Null),
        ("typdefault", Value::Null),
        ("typacl", Value::Null),
    ])
}
