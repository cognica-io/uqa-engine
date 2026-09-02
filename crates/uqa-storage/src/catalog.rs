//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Backend-neutral persistent catalog facade.
//!
//! The engine depends on this trait for table metadata, analyzers, models,
//! graph registries, and planner statistics. Concrete storage layers such as
//! `SQLite` or a future RocksDB-backed catalog implement it behind the same
//! object-safe boundary.

use serde::{Deserialize, Serialize};

use crate::backend::{StorageBackendError, StorageBackendResult};

/// Durable identity of a SQL relation.
///
/// The schema and local name are stored separately so `foo` and
/// `public.foo` can never become two physical catalog identities for the
/// same SQL object.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RelationIdentity {
    pub schema: String,
    pub name: String,
}

impl RelationIdentity {
    pub fn new(schema: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            schema: schema.into(),
            name: name.into(),
        }
    }

    pub fn qualified_name(&self) -> String {
        format!(
            "{}.{}",
            render_relation_component(&self.schema),
            render_relation_component(&self.name)
        )
    }

    /// Physical owner keys that can refer to this relation. New writes use
    /// only the canonical qualified name. Catalog cleanup also accepts the
    /// former unqualified key for `public` relations so data written before
    /// relation identities became schema-aware cannot survive its owner.
    pub(crate) fn canonical_and_legacy_public_names(&self) -> Vec<String> {
        let canonical = self.qualified_name();
        if self.schema != "public" {
            return vec![canonical];
        }
        let mut names = vec![canonical];
        let rendered_alias = render_relation_component(&self.name);
        if !names.contains(&rendered_alias) {
            names.push(rendered_alias);
        }
        // The direct Rust API historically accepted a decoded local name as
        // well as SQL-rendered text. Include that spelling only when parsing
        // it maps back to this exact relation; for example, raw `a.b` must not
        // be removed while dropping the distinct public relation `"a.b"`.
        if RelationIdentity::from_legacy_name(&self.name).is_ok_and(|raw| raw == *self)
            && !names.contains(&self.name)
        {
            names.push(self.name.clone());
        }
        names
    }

    /// Decode a SQL relation reference or a former flat catalog key.
    /// Unqualified objects belong to `public`. Quoted components preserve
    /// embedded dots and escaped quotes, so `public.\"a.b\"` is distinct from
    /// `\"public.a\".b` all the way down to physical storage keys.
    pub fn from_legacy_name(value: &str) -> Result<Self, String> {
        let (schema, name) = Self::parse_reference(value)?;
        Ok(Self::new(
            schema.unwrap_or_else(|| "public".to_string()),
            name,
        ))
    }

    /// Parse a possibly-unqualified SQL relation reference without choosing a
    /// search-path schema. Components use `PostgreSQL` double-quote escaping.
    pub fn parse_reference(value: &str) -> Result<(Option<String>, String), String> {
        let components = parse_relation_components(value)?;
        match components.as_slice() {
            [name] => Ok((None, name.clone())),
            [schema, name] => Ok((Some(schema.clone()), name.clone())),
            _ => Err(format!("invalid persisted relation name `{value}`")),
        }
    }
}

fn render_relation_component(component: &str) -> String {
    let can_render_bare = component
        .bytes()
        .enumerate()
        .all(|(index, byte)| match byte {
            b'a'..=b'z' | b'_' => true,
            b'0'..=b'9' | b'$' => index != 0,
            _ => false,
        });
    if can_render_bare && !component.is_empty() {
        component.to_string()
    } else {
        format!("\"{}\"", component.replace('"', "\"\""))
    }
}

fn parse_relation_components(value: &str) -> Result<Vec<String>, String> {
    if value.is_empty() {
        return Err("persisted relation name is empty".to_string());
    }
    let mut components = Vec::with_capacity(2);
    let mut chars = value.char_indices().peekable();
    while chars.peek().is_some() {
        let mut component = String::new();
        if chars.peek().is_some_and(|(_, ch)| *ch == '"') {
            chars.next();
            let mut terminated = false;
            while let Some((_, ch)) = chars.next() {
                if ch != '"' {
                    component.push(ch);
                    continue;
                }
                if chars.peek().is_some_and(|(_, next)| *next == '"') {
                    chars.next();
                    component.push('"');
                } else {
                    terminated = true;
                    break;
                }
            }
            if !terminated {
                return Err(format!("unterminated quoted relation name `{value}`"));
            }
            if chars.peek().is_some_and(|(_, ch)| *ch != '.') {
                return Err(format!("invalid persisted relation name `{value}`"));
            }
        } else {
            while let Some((_, ch)) = chars.peek() {
                if *ch == '.' {
                    break;
                }
                if *ch == '"' {
                    return Err(format!("invalid persisted relation name `{value}`"));
                }
                component.push(*ch);
                chars.next();
            }
        }
        if component.is_empty() {
            return Err(format!("invalid persisted relation name `{value}`"));
        }
        components.push(component);
        if components.len() > 2 {
            return Err(format!("invalid persisted relation name `{value}`"));
        }
        match chars.next() {
            Some((_, '.')) if chars.peek().is_some() => {}
            Some(_) => return Err(format!("invalid persisted relation name `{value}`")),
            None => break,
        }
    }
    Ok(components)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    Table,
    View,
    Sequence,
    ForeignTable,
}

impl RelationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Table => "table",
            Self::View => "view",
            Self::Sequence => "sequence",
            Self::ForeignTable => "foreign_table",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableSchema {
    pub relation: RelationIdentity,
    /// Stable logical relation identity. `CREATE TABLE` allocates a new value, while renames, schema changes, `TRUNCATE`, and reopen preserve it. A zero value marks a legacy catalog row that the engine upgrades during open.
    #[serde(default)]
    pub object_id: [u8; 16],
    /// Stable physical-storage identity. `CREATE TABLE` and `TRUNCATE` allocate a new value, while schema-only alterations and renames preserve it. A zero value marks a legacy catalog row that the engine upgrades during open.
    #[serde(default)]
    pub storage_generation: [u8; 16],
    pub analyzer_json: String,
    pub fts_fields: Vec<String>,
    pub vector_fields: Vec<VectorFieldSchema>,
    /// Serialized `Vec<uqa_sql::ast::ColumnDef>` capturing the schema
    /// columns (name, type, `auto_increment`, flags). Empty for
    /// tables created by the legacy code path before column tracking
    /// existed.
    #[serde(default)]
    pub columns_json: String,
    /// Serialized `uqa_sql::ast::TableConstraintSet`. Empty for catalogs
    /// created before durable table constraints were introduced.
    #[serde(default)]
    pub constraints_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorFieldSchema {
    pub field: String,
    pub dimensions: u32,
}

/// One row from graph-edge persistence, represented as a typed struct so the
/// catalog API stays explicit and clippy-clean.
#[derive(Debug, Clone)]
pub struct EdgeRow {
    pub edge_id: u64,
    pub source_id: u64,
    pub target_id: u64,
    pub label: String,
    pub properties_json: String,
}

/// One vertex in an atomic named-graph snapshot replacement.
#[derive(Debug, Clone)]
pub struct GraphVertexRow {
    pub vertex_id: u64,
    pub label: String,
    pub properties_json: String,
}

/// Complete persisted shape of one named graph. Catalog implementations
/// replace the graph membership and entity rows as one atomic operation.
#[derive(Debug, Clone)]
pub struct GraphSnapshot {
    pub vertices: Vec<GraphVertexRow>,
    pub edges: Vec<EdgeRow>,
    pub label_registry_json: String,
}

/// One row from the foreign-table registry.
#[derive(Debug, Clone)]
pub struct ForeignTableRow {
    pub relation: RelationIdentity,
    pub server_name: String,
    pub columns_json: String,
    pub options_json: String,
}

/// One durable view definition. `definition_json` contains a serialized
/// planner query plan, while ownership remains a typed catalog relation.
#[derive(Debug, Clone)]
pub struct ViewRow {
    pub relation: RelationIdentity,
    pub definition_json: String,
}

/// One row from the secondary-index registry.
#[derive(Debug, Clone)]
pub struct CatalogIndexRow {
    pub name: String,
    pub index_type: String,
    pub table_name: String,
    pub columns_json: String,
    pub parameters_json: String,
}

/// Values persisted into one column-statistics row.
#[derive(Debug, Clone, Copy)]
pub struct ColumnStatsInput<'a> {
    pub table_name: &'a str,
    pub column_name: &'a str,
    pub distinct_count: i64,
    pub null_count: i64,
    pub min_value: Option<&'a str>,
    pub max_value: Option<&'a str>,
    pub row_count: i64,
    pub histogram_json: &'a str,
    pub mcv_values_json: &'a str,
    pub mcv_frequencies_json: &'a str,
}

impl<'a> ColumnStatsInput<'a> {
    pub fn basic(
        table_name: &'a str,
        column_name: &'a str,
        distinct_count: i64,
        null_count: i64,
        min_value: Option<&'a str>,
        max_value: Option<&'a str>,
        row_count: i64,
    ) -> Self {
        Self {
            table_name,
            column_name,
            distinct_count,
            null_count,
            min_value,
            max_value,
            row_count,
            histogram_json: "[]",
            mcv_values_json: "[]",
            mcv_frequencies_json: "[]",
        }
    }
}

/// One row from persisted column statistics.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnStatsRow {
    pub column_name: String,
    pub distinct_count: i64,
    pub null_count: i64,
    pub min_value: Option<String>,
    pub max_value: Option<String>,
    pub row_count: i64,
    pub histogram_json: String,
    pub mcv_values_json: String,
    pub mcv_frequencies_json: String,
}

/// Durable SQL sequence state. Sequence allocation is implemented by the
/// catalog backend as one atomic mutation so independent engine sessions (and
/// independently opened engines) cannot return the same value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SequenceOptions {
    pub data_type: String,
    /// `None` is accepted only while decoding a legacy row and is resolved from its increment direction by the engine.
    pub min_value: Option<i64>,
    /// `None` is accepted only while decoding a legacy row and is resolved from its increment direction by the engine.
    pub max_value: Option<i64>,
    pub cycle: bool,
    #[serde(default = "default_sequence_cache_size")]
    pub cache_size: i64,
}

const fn default_sequence_cache_size() -> i64 {
    1
}

impl Default for SequenceOptions {
    fn default() -> Self {
        Self {
            data_type: "bigint".into(),
            min_value: None,
            max_value: None,
            cycle: false,
            cache_size: default_sequence_cache_size(),
        }
    }
}

/// Dependency strength of a sequence owner. Ordinary `OWNED BY` and `SERIAL` use an automatic dependency, while an identity column owns its sequence through an internal dependency that cannot be reassigned or dropped directly.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SequenceOwnerDependency {
    #[default]
    Automatic,
    Internal,
}

impl SequenceOwnerDependency {
    #[must_use]
    pub const fn catalog_code(self) -> &'static str {
        match self {
            Self::Automatic => "a",
            Self::Internal => "i",
        }
    }
}

/// Stable owner identity for a sequence dependency. Names are deliberately excluded so table and column renames do not require dependency rewrites.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SequenceOwner {
    pub table_object_id: [u8; 16],
    pub column_object_id: [u8; 16],
    #[serde(default)]
    pub dependency: SequenceOwnerDependency,
}

/// Grantable privileges carried by one sequence ACL path.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SequencePrivileges {
    #[serde(default)]
    pub select: bool,
    #[serde(default)]
    pub update: bool,
    #[serde(default)]
    pub usage: bool,
}

impl SequencePrivileges {
    pub const ALL: Self = Self {
        select: true,
        update: true,
        usage: true,
    };

    #[must_use]
    pub const fn is_empty(self) -> bool {
        !self.select && !self.update && !self.usage
    }

    #[must_use]
    pub const fn intersects(self, other: Self) -> bool {
        self.select && other.select || self.update && other.update || self.usage && other.usage
    }

    pub fn insert(&mut self, other: Self) {
        self.select |= other.select;
        self.update |= other.update;
        self.usage |= other.usage;
    }

    pub fn remove(&mut self, other: Self) {
        self.select &= !other.select;
        self.update &= !other.update;
        self.usage &= !other.usage;
    }
}

/// One explicit sequence ACL path. `None` on `SequenceRow::acl` retains `PostgreSQL`'s default owner-only privileges.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SequenceAclEntry {
    pub role: String,
    /// Legacy persisted entries without an explicit grantor originate from the sequence owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grantor: Option<String>,
    #[serde(default)]
    pub privileges: SequencePrivileges,
    #[serde(default)]
    pub grant_options: SequencePrivileges,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SequenceRow {
    pub relation: RelationIdentity,
    /// SQL role that owns the sequence. Legacy catalogs predate roles and therefore belong to the bootstrap role.
    #[serde(default = "default_sequence_role_owner")]
    pub role_owner: String,
    /// Explicit ACL entries. `None` represents `PostgreSQL`'s null default ACL, in which the owner has all ordinary privileges.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acl: Option<Vec<SequenceAclEntry>>,
    /// Stable identity of this sequence incarnation. Dropping and recreating the same qualified name must allocate a different value.
    #[serde(default)]
    pub object_id: [u8; 16],
    /// Changes for every successful definition-changing `ALTER SEQUENCE` while remaining stable across name lifecycle operations, value reservations, and `setval`.
    #[serde(default)]
    pub definition_generation: [u8; 16],
    pub start: i64,
    pub increment: i64,
    pub current: i64,
    /// False until the first allocation returns `current` verbatim.
    pub called: bool,
    /// Number of values whose advancement is already durable in the sequence log.
    #[serde(default)]
    pub log_count: i64,
    /// `PostgreSQL` `pg_class.relpersistence` code. Durable sequence rows accept only permanent (`p`) and unlogged (`u`) values.
    pub persistence: String,
    #[serde(default)]
    pub owner: Option<SequenceOwner>,
    #[serde(default)]
    pub options: SequenceOptions,
}

fn default_sequence_role_owner() -> String {
    "uqa".into()
}

/// Physical sequence position consumed by one atomic reservation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SequenceValuePosition {
    pub current: i64,
    pub called: bool,
    pub log_count: i64,
}

/// One atomic sequence reservation. `first_value` is returned immediately, while the remaining values through `last_value` belong to the allocating session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SequenceValueReservation {
    pub first_value: i64,
    pub last_value: i64,
    pub count: i64,
    pub log_count: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SequenceReservationResult {
    Missing,
    DefinitionChanged,
    Exhausted,
    Reserved(SequenceValueReservation),
}

/// Reserve up to `cache_size` values without crossing a sequence bound. Cycling is applied when selecting the first value of a new reservation, matching `PostgreSQL`'s boundary-truncated cache blocks.
#[must_use]
pub fn sequence_value_reservation(
    position: SequenceValuePosition,
    increment: i64,
    min_value: i64,
    max_value: i64,
    cycle: bool,
    cache_size: i64,
) -> Option<SequenceValueReservation> {
    let SequenceValuePosition {
        current,
        called,
        log_count,
    } = position;
    debug_assert_ne!(increment, 0);
    debug_assert!(cache_size > 0);
    let first_value = if called {
        match current
            .checked_add(increment)
            .filter(|value| (min_value..=max_value).contains(value))
        {
            Some(value) => value,
            None if cycle && increment > 0 => min_value,
            None if cycle => max_value,
            None => return None,
        }
    } else {
        current
    };
    let distance = if increment > 0 {
        i128::from(max_value) - i128::from(first_value)
    } else {
        i128::from(first_value) - i128::from(min_value)
    };
    let step = i128::from(increment).abs();
    let available = distance / step + 1;
    let count = available.min(i128::from(cache_size));
    let last_value = i128::from(first_value) + i128::from(increment) * (count - 1);
    let initial_count = i128::from(!called);
    let cache_fetch = i128::from(cache_size) - initial_count;
    let mut fetch = cache_fetch;
    let mut next_log_count = i128::from(log_count);
    if i128::from(log_count) < cache_fetch || !called {
        fetch += 32;
        next_log_count = fetch;
    }
    let fetched = fetch.min(available - initial_count);
    next_log_count -= fetched.min(cache_fetch);
    next_log_count -= fetch - fetched;
    Some(SequenceValueReservation {
        first_value,
        last_value: i64::try_from(last_value).expect("reserved sequence value stays in bounds"),
        count: i64::try_from(count).expect("reservation count cannot exceed cache size"),
        log_count: i64::try_from(next_log_count)
            .expect("persisted sequence log count cannot exceed the cache request"),
    })
}

/// Engine-facing catalog facade for persistent metadata.
pub trait CatalogFacade: Send + Sync {
    fn set_metadata(&self, key: &str, value: &str) -> StorageBackendResult<()>;
    fn get_metadata(&self, key: &str) -> StorageBackendResult<Option<String>>;
    fn fts_storage_was_reset(&self) -> bool {
        false
    }

    /// Atomically migrate the former flat relation namespace into typed,
    /// schema-owned catalog objects. Implementations must reject normalized
    /// or cross-kind collisions instead of merging either object.
    fn migrate_relation_namespace(&self) -> StorageBackendResult<()>;

    fn save_schema(&self, name: &str) -> StorageBackendResult<()>;
    fn drop_schema(&self, name: &str) -> StorageBackendResult<()>;
    fn load_schemas(&self) -> StorageBackendResult<Vec<String>>;

    fn save_table(&self, schema: &TableSchema) -> StorageBackendResult<()>;
    fn load_tables(&self) -> StorageBackendResult<Vec<TableSchema>>;
    fn drop_table(&self, name: &str) -> StorageBackendResult<()>;
    /// Remove a table definition and every catalog/data row owned by it as
    /// one atomic catalog operation.
    fn drop_table_and_data(&self, name: &str) -> StorageBackendResult<()>;
    fn purge_table_data(&self, name: &str) -> StorageBackendResult<()>;
    fn rename_table_data(&self, from: &str, to: &str) -> StorageBackendResult<()>;
    fn drop_column_data(&self, table_name: &str, column_name: &str) -> StorageBackendResult<()>;
    fn rename_column_data(
        &self,
        table_name: &str,
        from: &str,
        to: &str,
    ) -> StorageBackendResult<()>;

    fn save_model(&self, name: &str, json: &str) -> StorageBackendResult<()>;
    fn load_models(&self) -> StorageBackendResult<Vec<(String, String)>>;
    fn load_model(&self, name: &str) -> StorageBackendResult<Option<String>>;
    fn drop_model(&self, name: &str) -> StorageBackendResult<()>;

    fn save_scoring_params(&self, name: &str, params_json: &str) -> StorageBackendResult<()>;
    fn load_scoring_params(&self, name: &str) -> StorageBackendResult<Option<String>>;
    fn load_all_scoring_params(&self) -> StorageBackendResult<Vec<(String, String)>>;
    fn drop_scoring_params(&self, name: &str) -> StorageBackendResult<()>;

    fn create_sequence_row(&self, sequence: &SequenceRow) -> StorageBackendResult<bool>;
    fn replace_sequence_row(&self, sequence: &SequenceRow) -> StorageBackendResult<bool>;
    /// Atomically move one sequence catalog row and its shared relation claim while preserving object identity and physical value state.
    fn rename_sequence_row(&self, from: &str, to: &str) -> StorageBackendResult<bool>;
    fn drop_sequence_row(&self, name: &str) -> StorageBackendResult<bool>;
    fn load_sequence_rows(&self) -> StorageBackendResult<Vec<SequenceRow>>;
    fn reserve_sequence_values(
        &self,
        name: &str,
        object_id: [u8; 16],
        definition_generation: [u8; 16],
    ) -> StorageBackendResult<SequenceReservationResult>;
    /// Compatibility allocation API for callers that do not retain a session cache. A cached reservation's unused values are intentionally abandoned, just as when a `PostgreSQL` session disconnects.
    fn next_sequence_value(
        &self,
        name: &str,
        object_id: [u8; 16],
    ) -> StorageBackendResult<Option<i64>> {
        loop {
            let relation =
                RelationIdentity::from_legacy_name(name).map_err(StorageBackendError::Other)?;
            let Some(row) = self
                .load_sequence_rows()?
                .into_iter()
                .find(|row| row.relation == relation && row.object_id == object_id)
            else {
                return Ok(None);
            };
            match self.reserve_sequence_values(name, object_id, row.definition_generation)? {
                SequenceReservationResult::Reserved(reservation) => {
                    return Ok(Some(reservation.first_value));
                }
                SequenceReservationResult::DefinitionChanged => {}
                SequenceReservationResult::Missing => return Ok(None),
                SequenceReservationResult::Exhausted => {
                    return Err(StorageBackendError::Other(format!(
                        "sequence `{name}` exhausted"
                    )));
                }
            }
        }
    }
    fn set_sequence_value(
        &self,
        name: &str,
        object_id: [u8; 16],
        value: i64,
        called: bool,
        log_count: i64,
    ) -> StorageBackendResult<Option<i64>>;

    fn save_view(&self, view: &ViewRow) -> StorageBackendResult<()>;
    fn drop_view(&self, relation: &RelationIdentity) -> StorageBackendResult<bool>;
    fn load_views(&self) -> StorageBackendResult<Vec<ViewRow>>;

    fn save_named_graph(&self, name: &str) -> StorageBackendResult<()>;
    fn drop_named_graph(&self, name: &str) -> StorageBackendResult<()>;
    fn load_named_graphs(&self) -> StorageBackendResult<Vec<String>>;
    fn save_vertex(
        &self,
        vertex_id: u64,
        label: &str,
        properties_json: &str,
    ) -> StorageBackendResult<()>;
    fn delete_vertex(&self, vertex_id: u64) -> StorageBackendResult<()>;
    fn load_vertices(&self) -> StorageBackendResult<Vec<(u64, String, String)>>;
    fn save_edge(
        &self,
        edge_id: u64,
        source_id: u64,
        target_id: u64,
        label: &str,
        properties_json: &str,
    ) -> StorageBackendResult<()>;
    fn delete_edge(&self, edge_id: u64) -> StorageBackendResult<()>;
    fn load_edges(&self) -> StorageBackendResult<Vec<EdgeRow>>;
    fn save_graph_membership(
        &self,
        entity_type: &str,
        entity_id: u64,
        graph_name: &str,
    ) -> StorageBackendResult<()>;
    fn delete_graph_membership(
        &self,
        entity_type: &str,
        entity_id: u64,
        graph_name: &str,
    ) -> StorageBackendResult<()>;
    fn delete_graph_membership_for_graph(&self, graph_name: &str) -> StorageBackendResult<()>;
    fn load_graph_memberships(&self) -> StorageBackendResult<Vec<(String, u64, String)>>;
    fn purge_orphan_graph_entities(&self) -> StorageBackendResult<()>;
    fn replace_named_graph(
        &self,
        graph_name: &str,
        snapshot: &GraphSnapshot,
    ) -> StorageBackendResult<()>;
    fn drop_named_graph_data(&self, graph_name: &str) -> StorageBackendResult<()>;

    fn save_analyzer(&self, name: &str, config_json: &str) -> StorageBackendResult<()>;
    fn drop_analyzer(&self, name: &str) -> StorageBackendResult<()>;
    fn load_analyzers(&self) -> StorageBackendResult<Vec<(String, String)>>;

    fn save_table_field_analyzer(
        &self,
        table_name: &str,
        field: &str,
        phase: &str,
        analyzer_name: &str,
    ) -> StorageBackendResult<()>;
    fn replace_table_field_analyzer(
        &self,
        table_name: &str,
        field: &str,
        phase: &str,
        analyzer_name: &str,
    ) -> StorageBackendResult<()>;
    fn drop_table_field_analyzer_field(
        &self,
        table_name: &str,
        field: &str,
    ) -> StorageBackendResult<()>;
    fn drop_table_field_analyzers(&self, table_name: &str) -> StorageBackendResult<()>;
    fn load_table_field_analyzers(
        &self,
    ) -> StorageBackendResult<Vec<(String, String, String, String)>>;

    fn save_foreign_server(
        &self,
        name: &str,
        fdw_type: &str,
        options_json: &str,
    ) -> StorageBackendResult<()>;
    fn drop_foreign_server(&self, name: &str) -> StorageBackendResult<()>;
    fn load_foreign_servers(&self) -> StorageBackendResult<Vec<(String, String, String)>>;

    fn save_foreign_table(
        &self,
        relation: &RelationIdentity,
        server_name: &str,
        columns_json: &str,
        options_json: &str,
    ) -> StorageBackendResult<()>;
    fn drop_foreign_table(&self, relation: &RelationIdentity) -> StorageBackendResult<()>;
    fn load_foreign_tables(&self) -> StorageBackendResult<Vec<ForeignTableRow>>;

    fn save_catalog_index(
        &self,
        name: &str,
        index_type: &str,
        table_name: &str,
        columns_json: &str,
        parameters_json: &str,
    ) -> StorageBackendResult<()>;
    fn drop_catalog_index(&self, name: &str) -> StorageBackendResult<()>;
    fn drop_catalog_indexes_for_table(&self, table_name: &str) -> StorageBackendResult<()>;
    fn load_catalog_indexes(&self) -> StorageBackendResult<Vec<CatalogIndexRow>>;

    fn save_path_index(
        &self,
        graph_name: &str,
        label_sequences_json: &str,
    ) -> StorageBackendResult<()>;
    fn drop_path_index(&self, graph_name: &str) -> StorageBackendResult<()>;
    fn load_path_indexes(&self) -> StorageBackendResult<Vec<(String, String)>>;

    fn save_column_stats(&self, stats: ColumnStatsInput<'_>) -> StorageBackendResult<()>;
    /// Atomically replace the complete statistics snapshot for one table.
    /// Implementations must leave the prior snapshot intact if any row fails.
    fn replace_column_stats(
        &self,
        table_name: &str,
        stats: &[ColumnStatsInput<'_>],
    ) -> StorageBackendResult<()>;
    fn load_column_stats(&self, table_name: &str) -> StorageBackendResult<Vec<ColumnStatsRow>>;
    fn delete_column_stats(&self, table_name: &str) -> StorageBackendResult<()>;
}

#[cfg(test)]
mod tests {
    use super::{
        sequence_value_reservation, RelationIdentity, SequenceValuePosition,
        SequenceValueReservation,
    };

    const fn sequence_position(
        current: i64,
        called: bool,
        log_count: i64,
    ) -> SequenceValuePosition {
        SequenceValuePosition {
            current,
            called,
            log_count,
        }
    }

    #[test]
    fn relation_identity_rendering_is_reversible_and_collision_free() {
        let left = RelationIdentity::new("a.b", "c");
        let right = RelationIdentity::new("a", "b.c");
        assert_eq!(left.qualified_name(), "\"a.b\".c");
        assert_eq!(right.qualified_name(), "a.\"b.c\"");
        assert_ne!(left.qualified_name(), right.qualified_name());
        assert_eq!(
            RelationIdentity::from_legacy_name(&left.qualified_name()).unwrap(),
            left
        );
        assert_eq!(
            RelationIdentity::from_legacy_name(&right.qualified_name()).unwrap(),
            right
        );
    }

    #[test]
    fn relation_identity_preserves_quotes_and_unqualified_public_alias() {
        let quoted = RelationIdentity::new("public", "a\"b.c");
        assert_eq!(quoted.qualified_name(), "public.\"a\"\"b.c\"");
        assert_eq!(
            quoted.canonical_and_legacy_public_names(),
            vec![
                "public.\"a\"\"b.c\"".to_string(),
                "\"a\"\"b.c\"".to_string()
            ]
        );
        assert_eq!(
            RelationIdentity::from_legacy_name(&quoted.qualified_name()).unwrap(),
            quoted
        );
        assert_eq!(
            RelationIdentity::from_legacy_name("plain").unwrap(),
            RelationIdentity::new("public", "plain")
        );
        assert_eq!(
            RelationIdentity::new("app", "plain").canonical_and_legacy_public_names(),
            vec!["app.plain".to_string()]
        );
        assert_eq!(
            RelationIdentity::new("public", "Upper").canonical_and_legacy_public_names(),
            vec![
                "public.\"Upper\"".to_string(),
                "\"Upper\"".to_string(),
                "Upper".to_string()
            ]
        );
    }

    #[test]
    fn sequence_reservations_track_postgresql_log_counts() {
        assert_eq!(
            sequence_value_reservation(sequence_position(1, false, 0), 1, 1, i64::MAX, false, 1),
            Some(SequenceValueReservation {
                first_value: 1,
                last_value: 1,
                count: 1,
                log_count: 32,
            })
        );
        assert_eq!(
            sequence_value_reservation(sequence_position(1, true, 32), 1, 1, i64::MAX, false, 1),
            Some(SequenceValueReservation {
                first_value: 2,
                last_value: 2,
                count: 1,
                log_count: 31,
            })
        );
        assert_eq!(
            sequence_value_reservation(sequence_position(1, false, 0), 1, 1, i64::MAX, false, 10),
            Some(SequenceValueReservation {
                first_value: 1,
                last_value: 10,
                count: 10,
                log_count: 32,
            })
        );
        assert_eq!(
            sequence_value_reservation(sequence_position(5, false, 0), 2, 3, 9, true, 3),
            Some(SequenceValueReservation {
                first_value: 5,
                last_value: 9,
                count: 3,
                log_count: 0,
            })
        );
        assert_eq!(
            sequence_value_reservation(
                sequence_position(1, false, 0),
                1,
                1,
                i64::MAX,
                false,
                i64::MAX,
            ),
            Some(SequenceValueReservation {
                first_value: 1,
                last_value: i64::MAX,
                count: i64::MAX,
                log_count: 0,
            })
        );
    }
}
