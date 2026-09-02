//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CREATE INDEX execution and index option validation.

use super::{
    ddl_storage_error, ColumnType, CreateIndex, Engine, HNSWIndexParams, IVFIndexParams, SQLError,
    SQLResult, VectorIndexSpec,
};

pub(in crate::sql) fn run_create_index(
    engine: &Engine,
    c: CreateIndex,
) -> Result<SQLResult, SQLError> {
    let table = engine.require_table(&c.table)?;
    if table.persistence == uqa_sql::ast::RelationPersistence::Temporary {
        engine.ensure_temporary_relation_creation_privilege()?;
    }
    // Every accepted access method has a matching physical implementation.
    // Reject unknown methods before allocating a name or mutating any table,
    // index, analyzer, or catalog state.
    let am = c.access_method.to_ascii_lowercase();
    if !matches!(am.as_str(), "" | "btree" | "gin" | "ivf" | "hnsw") {
        return Err(SQLError::Unsupported(format!(
            "CREATE INDEX access method `{}` is not supported",
            c.access_method
        )));
    }

    let name = if let Some(name) = c.name.as_ref() {
        if engine
            .has_catalog_index(name)
            .map_err(|err| ddl_storage_error("CREATE INDEX", err))?
        {
            if c.if_not_exists {
                return Ok(SQLResult::empty());
            }
            return Err(SQLError::Unsupported(format!(
                "Index `{name}` already exists"
            )));
        }
        name.clone()
    } else {
        allocate_default_index_name(engine, &c.table, &c.columns)?
    };

    validate_index_columns(engine, &c)?;

    match am.as_str() {
        "gin" => {
            for col in &c.columns {
                let analyzer = c
                    .options
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("analyzer"))
                    .map(|(_, v)| v.as_str());
                if let Err(e) = engine.add_fts_field_with_analyzer(&c.table, col.clone(), analyzer)
                {
                    return Err(SQLError::Internal(format!("add_fts_field: {e}")));
                }
            }
        }
        "" | "btree" => {}
        "ivf" | "hnsw" => create_vector_index(engine, &c, &am)?,
        _ => unreachable!("access method was validated above"),
    }
    // Persist the CREATE INDEX statement itself so reopen sees the
    // same set of registered indexes. The engine layer parses
    // `parameters_json` back into `(key, value)` pairs and re-runs
    // any access-method-specific side effects (e.g. add_fts_field
    // for `gin`) on restore.
    let catalog_index_type = if am.is_empty() { "btree" } else { &am };
    engine
        .try_register_catalog_index(&name, catalog_index_type, &c.table, &c.columns, &c.options)
        .map_err(|e| ddl_storage_error("CREATE INDEX", e))?;
    Ok(SQLResult::empty())
}

fn validate_index_columns(engine: &Engine, statement: &CreateIndex) -> Result<(), SQLError> {
    let definitions = engine
        .try_describe_table(&statement.table)
        .map_err(|error| ddl_storage_error("CREATE INDEX", error))?
        .ok_or_else(|| {
            SQLError::Unsupported(format!(
                "CREATE INDEX: relation `{}` does not exist",
                statement.table
            ))
        })?;
    if definitions.is_empty() {
        return Ok(());
    }
    for name in &statement.columns {
        let Some(column) = definitions.iter().find(|column| &column.name == name) else {
            return Err(SQLError::Unsupported(format!(
                "CREATE INDEX: column `{}`.`{name}` does not exist",
                statement.table
            )));
        };
        if column
            .generated
            .as_ref()
            .is_some_and(|generated| generated.kind == uqa_sql::ast::GeneratedColumnKind::Virtual)
        {
            return Err(SQLError::TypeMismatch(format!(
                "indexes on virtual generated column `{name}` are not supported"
            )));
        }
    }
    Ok(())
}

fn create_vector_index(
    engine: &Engine,
    statement: &CreateIndex,
    access_method: &str,
) -> Result<(), SQLError> {
    let spec = match access_method {
        "ivf" => VectorIndexSpec::IVF(parse_ivf_index_params(&statement.options)?),
        "hnsw" => VectorIndexSpec::HNSW(parse_hnsw_index_params(&statement.options)?),
        _ => unreachable!("vector access method was validated above"),
    };
    let table = engine
        .try_resolve_table_name(&statement.table)
        .map_err(|err| ddl_storage_error("CREATE INDEX", err))?
        .ok_or_else(|| {
            SQLError::Unsupported(format!(
                "CREATE INDEX USING {access_method}: relation `{}` does not exist",
                statement.table
            ))
        })?;
    let mut fields = Vec::with_capacity(statement.columns.len());
    for column in &statement.columns {
        let dimensions = match engine
            .column_type(&table, column)
            .map_err(|err| ddl_storage_error("CREATE INDEX", err))?
        {
            Some(ColumnType::Vector(dim) | ColumnType::Tensor(dim)) => dim,
            Some(other) => {
                return Err(SQLError::Unsupported(format!(
                    "CREATE INDEX USING {access_method} requires VECTOR or TENSOR column `{column}`, got {other:?}"
                )));
            }
            None => {
                return Err(SQLError::Unsupported(format!(
                    "CREATE INDEX USING {access_method}: column `{table}`.`{column}` does not exist"
                )));
            }
        };
        let existing = engine
            .vector_catalog_index_names_for_column(&table, column)
            .map_err(|err| ddl_storage_error("CREATE INDEX", err))?;
        if !existing.is_empty() {
            return Err(SQLError::Unsupported(format!(
                "CREATE INDEX USING {access_method}: `{table}`.`{column}` already has physical vector index `{}`",
                existing.join("`, `")
            )));
        }
        fields.push((column, dimensions));
    }
    for (column, dimensions) in fields {
        if !engine
            .rebuild_vector_field_with_spec(&table, column.clone(), dimensions, spec)
            .map_err(|err| ddl_storage_error("CREATE INDEX vector field", err))?
        {
            return Err(SQLError::Unsupported(format!(
                "CREATE INDEX USING {access_method}: relation `{table}` does not exist"
            )));
        }
    }
    Ok(())
}

fn allocate_default_index_name(
    engine: &Engine,
    table: &str,
    columns: &[String],
) -> Result<String, SQLError> {
    fn component(raw: &str) -> String {
        let mut out = String::with_capacity(raw.len());
        let mut previous_was_separator = false;
        for ch in raw.chars() {
            if ch.is_alphanumeric() || ch == '_' {
                out.extend(ch.to_lowercase());
                previous_was_separator = false;
            } else if !previous_was_separator && !out.is_empty() {
                out.push('_');
                previous_was_separator = true;
            }
        }
        while out.ends_with('_') {
            out.pop();
        }
        out
    }

    let mut parts = table
        .split('.')
        .map(component)
        .chain(columns.iter().map(|column| component(column)))
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.is_empty() {
        parts.push("index".to_string());
    }
    let base = format!("{}_idx", parts.join("_"));
    let existing = engine
        .list_catalog_indexes()
        .map_err(|err| ddl_storage_error("CREATE INDEX", err))?
        .into_iter()
        .map(|row| row.name)
        .collect::<std::collections::BTreeSet<_>>();
    if !existing.contains(&base) {
        return Ok(base);
    }
    for suffix in 1_u64.. {
        let candidate = format!("{base}_{suffix}");
        if !existing.contains(&candidate) {
            return Ok(candidate);
        }
    }
    unreachable!("u64 index-name suffix space is non-empty")
}

fn parse_ivf_index_params(options: &[(String, String)]) -> Result<IVFIndexParams, SQLError> {
    let mut params = IVFIndexParams::default();
    let mut seen = std::collections::BTreeSet::new();
    for (key, value) in options {
        if key.eq_ignore_ascii_case("lists") || key.eq_ignore_ascii_case("nlist") {
            claim_index_option(&mut seen, "nlist", "ivf", key)?;
            params.nlist = parse_positive_usize_option("ivf", key, value)?;
        } else if key.eq_ignore_ascii_case("probes") || key.eq_ignore_ascii_case("nprobe") {
            claim_index_option(&mut seen, "nprobe", "ivf", key)?;
            params.nprobe = parse_positive_usize_option("ivf", key, value)?;
        } else if key.eq_ignore_ascii_case("train_threshold")
            || key.eq_ignore_ascii_case("train-threshold")
            || key.eq_ignore_ascii_case("min_train")
        {
            claim_index_option(&mut seen, "train_threshold", "ivf", key)?;
            params.train_threshold = parse_positive_usize_option("ivf", key, value)?;
        } else {
            return Err(SQLError::Unsupported(format!(
                "CREATE INDEX USING ivf option `{key}` is not supported"
            )));
        }
    }
    Ok(params)
}

fn parse_hnsw_index_params(options: &[(String, String)]) -> Result<HNSWIndexParams, SQLError> {
    let mut params = HNSWIndexParams::default();
    let mut seen = std::collections::BTreeSet::new();
    for (key, value) in options {
        if key.eq_ignore_ascii_case("m") {
            claim_index_option(&mut seen, "m", "hnsw", key)?;
            params.m = parse_positive_usize_option("hnsw", key, value)?;
        } else if key.eq_ignore_ascii_case("ef_construction")
            || key.eq_ignore_ascii_case("ef-construction")
        {
            claim_index_option(&mut seen, "ef_construction", "hnsw", key)?;
            params.ef_construction = parse_positive_usize_option("hnsw", key, value)?;
        } else if key.eq_ignore_ascii_case("ef_search") || key.eq_ignore_ascii_case("ef-search") {
            claim_index_option(&mut seen, "ef_search", "hnsw", key)?;
            params.ef_search = parse_positive_usize_option("hnsw", key, value)?;
        } else if key.eq_ignore_ascii_case("rebuild_threshold")
            || key.eq_ignore_ascii_case("rebuild-threshold")
        {
            claim_index_option(&mut seen, "rebuild_threshold", "hnsw", key)?;
            params.rebuild_threshold = parse_positive_usize_option("hnsw", key, value)?;
        } else if key.eq_ignore_ascii_case("seed") {
            claim_index_option(&mut seen, "seed", "hnsw", key)?;
            params.seed = value.parse::<u64>().map_err(|_| {
                SQLError::TypeMismatch(format!(
                    "CREATE INDEX USING hnsw option `{key}` must be an unsigned integer"
                ))
            })?;
        } else {
            return Err(SQLError::Unsupported(format!(
                "CREATE INDEX USING hnsw option `{key}` is not supported"
            )));
        }
    }
    params
        .validate()
        .map_err(|error| SQLError::TypeMismatch(format!("CREATE INDEX USING hnsw: {error}")))
}

fn claim_index_option(
    seen: &mut std::collections::BTreeSet<&'static str>,
    canonical: &'static str,
    access_method: &str,
    source: &str,
) -> Result<(), SQLError> {
    if !seen.insert(canonical) {
        return Err(SQLError::Unsupported(format!(
            "CREATE INDEX USING {access_method} option `{source}` duplicates `{canonical}`"
        )));
    }
    Ok(())
}

fn parse_positive_usize_option(
    access_method: &str,
    key: &str,
    value: &str,
) -> Result<usize, SQLError> {
    let parsed = value.parse::<usize>().map_err(|_| {
        SQLError::TypeMismatch(format!(
            "CREATE INDEX USING {access_method} option `{key}` must be a positive integer"
        ))
    })?;
    if parsed == 0 {
        return Err(SQLError::TypeMismatch(format!(
            "CREATE INDEX USING {access_method} option `{key}` must be a positive integer"
        )));
    }
    Ok(parsed)
}
