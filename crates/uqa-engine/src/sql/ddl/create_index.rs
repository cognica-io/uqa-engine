//! CREATE INDEX execution and index option validation.

use super::{
    ddl_storage_error, ColumnType, CreateIndex, Engine, IVFIndexParams, SQLError, SQLResult,
};

pub(in crate::sql) fn run_create_index(
    engine: &Engine,
    c: CreateIndex,
) -> Result<SQLResult, SQLError> {
    // Every accepted access method has a matching physical implementation.
    // Reject unknown methods before allocating a name or touching any table,
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
        "ivf" | "hnsw" => {
            let params = parse_ivf_index_params(&c.options)?;
            for col in &c.columns {
                match engine
                    .column_type(&c.table, col)
                    .map_err(|err| ddl_storage_error("CREATE INDEX", err))?
                {
                    Some(ColumnType::Vector(dim) | ColumnType::Tensor(dim)) => {
                        if !engine
                            .rebuild_ivf_vector_field(&c.table, col.clone(), dim, params)
                            .map_err(|err| ddl_storage_error("CREATE INDEX vector field", err))?
                        {
                            return Err(SQLError::Unsupported(format!(
                                "CREATE INDEX USING ivf: relation `{}` does not exist",
                                c.table
                            )));
                        }
                    }
                    Some(other) => {
                        return Err(SQLError::Unsupported(format!(
                            "CREATE INDEX USING ivf requires VECTOR or TENSOR column `{col}`, got {other:?}"
                        )));
                    }
                    None => {
                        return Err(SQLError::Unsupported(format!(
                            "CREATE INDEX USING ivf: column `{}`.`{col}` does not exist",
                            c.table
                        )));
                    }
                }
            }
        }
        _ => unreachable!("access method was validated above"),
    }
    // Persist the CREATE INDEX statement itself so reopen sees the
    // same set of registered indexes. The engine layer parses
    // `parameters_json` back into `(key, value)` pairs and re-runs
    // any access-method-specific side effects (e.g. add_fts_field
    // for `gin`) on restore.
    let catalog_index_type = match am.as_str() {
        "" => "btree",
        "hnsw" => "ivf",
        other => other,
    };
    engine
        .try_register_catalog_index(&name, catalog_index_type, &c.table, &c.columns, &c.options)
        .map_err(|e| ddl_storage_error("CREATE INDEX", e))?;
    Ok(SQLResult::empty())
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
    for (key, value) in options {
        if key.eq_ignore_ascii_case("lists") || key.eq_ignore_ascii_case("nlist") {
            params.nlist = parse_positive_usize_option(key, value)?;
        } else if key.eq_ignore_ascii_case("probes") || key.eq_ignore_ascii_case("nprobe") {
            params.nprobe = parse_positive_usize_option(key, value)?;
        } else if key.eq_ignore_ascii_case("train_threshold")
            || key.eq_ignore_ascii_case("train-threshold")
            || key.eq_ignore_ascii_case("min_train")
        {
            params.train_threshold = parse_positive_usize_option(key, value)?;
        } else {
            return Err(SQLError::Unsupported(format!(
                "CREATE INDEX USING ivf option `{key}` is not supported"
            )));
        }
    }
    Ok(params)
}

fn parse_positive_usize_option(key: &str, value: &str) -> Result<usize, SQLError> {
    let parsed = value.parse::<usize>().map_err(|_| {
        SQLError::TypeMismatch(format!(
            "CREATE INDEX USING ivf option `{key}` must be a positive integer"
        ))
    })?;
    if parsed == 0 {
        return Err(SQLError::TypeMismatch(format!(
            "CREATE INDEX USING ivf option `{key}` must be a positive integer"
        )));
    }
    Ok(parsed)
}
