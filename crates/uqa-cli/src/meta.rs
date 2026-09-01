//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Backslash command dispatch and catalog inspection commands.

use super::*;

impl Session {
    #[expect(
        clippy::too_many_lines,
        reason = "preserves meta-command output contracts"
    )]
    pub(super) fn handle_meta(&mut self, command: &str, out: &mut impl Write) -> PromptLineOutcome {
        let mut parts = command.trim().splitn(2, char::is_whitespace);
        let cmd = parts.next().unwrap_or("");
        let arg = parts.next().unwrap_or("").trim();
        let result = match cmd {
            "q" | "quit" | "exit" => return PromptLineOutcome::Quit,
            "?" | "help" | "h" => {
                print_backslash_help(out);
                Ok(())
            }
            "history" => match arg {
                "" => {
                    for line in &self.history {
                        let _ = writeln!(out, "{line}");
                    }
                    Ok(())
                }
                "clear" => {
                    let removal = self.history_path.as_ref().map_or(Ok(()), |path| {
                        std::fs::remove_file(path).or_else(|error| {
                            if error.kind() == io::ErrorKind::NotFound {
                                Ok(())
                            } else {
                                Err(error)
                            }
                        })
                    });
                    match removal {
                        Ok(()) => {
                            self.history.clear();
                            let _ = writeln!(out, "history cleared");
                            Ok(())
                        }
                        Err(error) => Err(format!("history clear failed: {error}")),
                    }
                }
                other => Err(format!("usage: \\history [clear] (got {other:?})")),
            },
            "open" => {
                if arg.is_empty() {
                    Err("usage: \\open <path>".into())
                } else {
                    match open_engine_with_key(Path::new(arg), None) {
                        Ok((engine, location, key)) => {
                            self.engine = engine;
                            self.db_path = Some(PathBuf::from(arg));
                            self.db_key = key;
                            self.location = location;
                            let _ = writeln!(out, "opened {arg}");
                            Ok(())
                        }
                        Err(err) => Err(err),
                    }
                }
            }
            "new" => {
                self.engine = Engine::new();
                self.db_path = None;
                self.db_key = None;
                self.location = ":memory:".into();
                let _ = writeln!(out, "fresh in-memory engine");
                Ok(())
            }
            "reset" => match open_engine(self.db_path.as_deref(), self.db_key.as_deref()) {
                Ok((engine, location, key)) => {
                    self.engine = engine;
                    self.db_key = key;
                    self.location = location;
                    let _ = writeln!(out, "Engine reset.");
                    Ok(())
                }
                Err(err) => Err(format!("reset failed: {err}")),
            },
            "where" => {
                let _ = writeln!(out, "{}", self.location);
                Ok(())
            }
            "timing" => {
                self.show_timing = !self.show_timing;
                let state = if self.show_timing { "on" } else { "off" };
                let _ = writeln!(out, "Timing is {state}.");
                Ok(())
            }
            "expanded" | "x" => {
                self.expanded = !self.expanded;
                let state = if self.expanded { "on" } else { "off" };
                let _ = writeln!(out, "Expanded display is {state}.");
                Ok(())
            }
            "o" => self.handle_output_redirect(arg, out),
            "dt" | "tables" => self.cmd_list_tables(out),
            "describe" | "d" => self.cmd_describe_table(arg, out),
            "di" => self.cmd_list_indexes(out),
            "stats" => self.cmd_show_stats(arg, out),
            "ds" => self.cmd_list_sequences(arg, out),
            "dg" | "graphs" => self.cmd_list_graphs(out),
            "dfs" | "dS" => self.cmd_list_foreign_servers(out),
            "dft" | "dF" => self.cmd_list_foreign_tables(out),
            "da" | "analyzers" => self
                .engine
                .list_named_analyzers()
                .map_err(|err| format!("Failed to read analyzers: {err}"))
                .map(|names| {
                    if names.is_empty() {
                        let _ = writeln!(out, "no analyzers registered");
                    } else {
                        for name in names {
                            let _ = writeln!(out, "  {name}");
                        }
                    }
                }),
            "run" => {
                if arg.is_empty() {
                    Err("usage: \\run <file>".into())
                } else {
                    self.run_file(Path::new(arg), out)
                }
            }
            "migrate-python-db" => self.handle_migrate_python_db(arg, out),
            other => {
                print_backslash_help(out);
                Err(format!("unknown command: \\{other}"))
            }
        };
        match result {
            Ok(()) => PromptLineOutcome::Continue,
            Err(error) => {
                let _ = writeln!(out, "ERROR: {error}");
                PromptLineOutcome::Failed
            }
        }
    }

    pub(super) fn handle_output_redirect(
        &mut self,
        arg: &str,
        out: &mut impl Write,
    ) -> Result<(), String> {
        if arg.is_empty() {
            if let Some(path) = self.output_path.take() {
                let _ = writeln!(out, "Output restored to stdout (was: {}).", path.display());
            } else {
                let _ = writeln!(out, "Output already goes to stdout.");
            }
            return Ok(());
        }
        let path = PathBuf::from(arg);
        match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(_) => {
                self.output_path = Some(path);
                let _ = writeln!(out, "Output redirected to: {arg}");
                Ok(())
            }
            Err(error) => Err(format!("Output redirection failed for {arg}: {error}")),
        }
    }

    pub(super) fn cmd_list_tables(&self, out: &mut impl Write) -> Result<(), String> {
        let table_names = self
            .engine
            .table_names()
            .map_err(|err| format!("Failed to read tables: {err}"))?;
        let mut rows = Vec::new();
        for name in table_names {
            let columns = match self.engine.describe_table(&name) {
                Ok(Some(columns)) => usize_count_value(columns.len()),
                Ok(None) => {
                    return Err(format!(
                        "table '{name}' disappeared while listing its metadata"
                    ));
                }
                Err(err) => return Err(format!("Failed to describe table '{name}': {err}")),
            };
            let row_count = self
                .engine
                .table_doc_ids(&name)
                .map(|doc_ids| usize_count_value(doc_ids.len()))
                .map_err(|err| format!("Failed to count rows in '{name}': {err}"))?;
            rows.push(result_row(vec![
                ("table_name", Value::Str(name.clone())),
                ("type", Value::Str("table".into())),
                ("columns", columns),
                ("rows", row_count),
            ]));
        }
        let foreign_tables = self
            .engine
            .list_foreign_tables()
            .map_err(|err| format!("Failed to read foreign tables: {err}"))?;
        for name in foreign_tables {
            match self.engine.foreign_table(&name) {
                Ok(Some(table)) => {
                    rows.push(result_row(vec![
                        ("table_name", Value::Str(name)),
                        ("type", Value::Str("foreign".into())),
                        ("columns", usize_count_value(table.columns.len())),
                        ("rows", Value::Str(String::new())),
                    ]));
                }
                Ok(None) => {
                    return Err(format!(
                        "foreign table '{name}' disappeared while listing its metadata"
                    ));
                }
                Err(err) => return Err(format!("Failed to read foreign table '{name}': {err}")),
            }
        }
        if rows.is_empty() {
            let _ = writeln!(out, "No tables.");
            return Ok(());
        }
        print_result(
            &SQLResult::from_rows(
                vec![
                    "table_name".into(),
                    "type".into(),
                    "columns".into(),
                    "rows".into(),
                ],
                rows,
            ),
            out,
        );
        Ok(())
    }

    pub(super) fn cmd_describe_table(
        &self,
        name: &str,
        out: &mut impl Write,
    ) -> Result<(), String> {
        if name.is_empty() {
            return Err("Usage: \\d <table_name>".into());
        }
        match self.engine.describe_table(name) {
            Ok(Some(cols)) => {
                let _ = writeln!(out, "Table \"{name}\"");
                print_columns(&cols, out);
                return Ok(());
            }
            Ok(None) => {}
            Err(err) => return Err(format!("Failed to describe table '{name}': {err}")),
        }
        match self.engine.foreign_table(name) {
            Ok(Some(table)) => {
                let _ = writeln!(
                    out,
                    "Foreign table \"{name}\" (server: {})",
                    table.server_name
                );
                let rows = table
                    .columns
                    .iter()
                    .map(|col| {
                        result_row(vec![
                            ("column", Value::Str(col.name.clone())),
                            ("type", Value::Str(fdw_type_name(&col.ty))),
                            ("constraints", Value::Str(String::new())),
                        ])
                    })
                    .collect();
                print_result(
                    &SQLResult::from_rows(
                        vec!["column".into(), "type".into(), "constraints".into()],
                        rows,
                    ),
                    out,
                );
                return Ok(());
            }
            Ok(None) => {}
            Err(err) => return Err(format!("Failed to read foreign table '{name}': {err}")),
        }
        Err(format!("Table '{name}' does not exist."))
    }

    pub(super) fn cmd_list_indexes(&self, out: &mut impl Write) -> Result<(), String> {
        let table_names = self
            .engine
            .table_names()
            .map_err(|err| format!("Failed to read tables: {err}"))?;
        if table_names.is_empty() {
            let _ = writeln!(out, "No tables.");
            return Ok(());
        }
        let mut by_table: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let stats = self
            .engine
            .fts_index_stats(None)
            .map_err(|err| format!("Failed to read index statistics: {err}"))?;
        for stat in stats {
            by_table
                .entry(stat.table_name)
                .or_default()
                .push(stat.field);
        }
        if by_table.is_empty() {
            let _ = writeln!(out, "No indexed fields.");
            return Ok(());
        }
        let rows = by_table
            .into_iter()
            .map(|(table, mut fields)| {
                fields.sort();
                result_row(vec![
                    ("table_name", Value::Str(table)),
                    ("indexed_fields", Value::Str(fields.join(", "))),
                ])
            })
            .collect();
        print_result(
            &SQLResult::from_rows(vec!["table_name".into(), "indexed_fields".into()], rows),
            out,
        );
        Ok(())
    }

    pub(super) fn cmd_show_stats(&self, name: &str, out: &mut impl Write) -> Result<(), String> {
        if name.is_empty() {
            return Err("Usage: \\stats <table_name>".into());
        }
        match self.engine.describe_table(name) {
            Ok(Some(_)) => {}
            Ok(None) => return Err(format!("Table '{name}' does not exist.")),
            Err(err) => return Err(format!("Failed to describe table '{name}': {err}")),
        }
        let stats = self
            .engine
            .column_stats(name)
            .map_err(|err| format!("Failed to read statistics for '{name}': {err}"))?;
        if stats.is_empty() {
            let _ = writeln!(out, "No statistics for '{name}' (no declared columns).");
            return Ok(());
        }
        let row_count = stats.values().next().map_or(0, |s| s.row_count);
        let _ = writeln!(out, "Statistics for \"{name}\" ({row_count} rows)");
        let rows = stats
            .into_iter()
            .map(|(col, s)| {
                result_row(vec![
                    ("column", Value::Str(col)),
                    ("distinct", u64_count_value(s.distinct_count)),
                    ("nulls", u64_count_value(s.null_count)),
                    ("min", optional_value_to_display_value(s.min_value.as_ref())),
                    ("max", optional_value_to_display_value(s.max_value.as_ref())),
                    ("selectivity", Value::Float(s.equality_selectivity())),
                ])
            })
            .collect();
        print_result(
            &SQLResult::from_rows(
                vec![
                    "column".into(),
                    "distinct".into(),
                    "nulls".into(),
                    "min".into(),
                    "max".into(),
                    "selectivity".into(),
                ],
                rows,
            ),
            out,
        );
        Ok(())
    }

    pub(super) fn cmd_list_sequences(
        &self,
        name: &str,
        out: &mut impl Write,
    ) -> Result<(), String> {
        let rows: Vec<_> = if name.is_empty() {
            self.engine
                .try_sequences_snapshot()
                .map_err(|error| format!("Failed to read sequences: {error}"))?
                .into_iter()
                .map(sequence_row)
                .collect()
        } else {
            match self.engine.sequence_state(name) {
                Ok(Some((canonical, state))) => vec![sequence_row((canonical, state))],
                Ok(None) => return Err(format!("Sequence '{name}' does not exist.")),
                Err(error) => {
                    return Err(format!("Failed to read sequence '{name}': {error}"));
                }
            }
        };

        if rows.is_empty() {
            let _ = writeln!(out, "No sequences.");
            return Ok(());
        }
        print_result(
            &SQLResult::from_rows(
                vec![
                    "sequence_name".into(),
                    "start".into(),
                    "increment".into(),
                    "current".into(),
                ],
                rows,
            ),
            out,
        );
        Ok(())
    }

    pub(super) fn cmd_list_foreign_tables(&self, out: &mut impl Write) -> Result<(), String> {
        let names = self
            .engine
            .list_foreign_tables()
            .map_err(|err| format!("Failed to read foreign tables: {err}"))?;
        if names.is_empty() {
            let _ = writeln!(out, "No foreign tables.");
            return Ok(());
        }
        let mut rows = Vec::new();
        for name in names {
            match self.engine.foreign_table(&name) {
                Ok(Some(table)) => {
                    let options = foreign_table_options_display(&table.options);
                    let source = table.options.get("source").cloned().unwrap_or_default();
                    rows.push(result_row(vec![
                        ("table_name", Value::Str(table.name)),
                        ("server", Value::Str(table.server_name)),
                        ("columns", usize_count_value(table.columns.len())),
                        ("source", Value::Str(source)),
                        ("options", Value::Str(options)),
                    ]));
                }
                Ok(None) => {
                    return Err(format!(
                        "foreign table '{name}' disappeared while listing its metadata"
                    ));
                }
                Err(err) => return Err(format!("Failed to read foreign table '{name}': {err}")),
            }
        }
        print_result(
            &SQLResult::from_rows(
                vec![
                    "table_name".into(),
                    "server".into(),
                    "columns".into(),
                    "source".into(),
                    "options".into(),
                ],
                rows,
            ),
            out,
        );
        Ok(())
    }

    pub(super) fn cmd_list_foreign_servers(&self, out: &mut impl Write) -> Result<(), String> {
        let names = self
            .engine
            .list_foreign_servers()
            .map_err(|err| format!("Failed to read foreign servers: {err}"))?;
        if names.is_empty() {
            let _ = writeln!(out, "No foreign servers.");
            return Ok(());
        }
        let mut rows = Vec::new();
        for name in names {
            match self.engine.foreign_server(&name) {
                Ok(Some(server)) => rows.push(result_row(vec![
                    ("server_name", Value::Str(server.name)),
                    ("fdw_type", Value::Str(server.fdw_type)),
                    ("options", Value::Str(options_display(&server.options))),
                ])),
                Ok(None) => {
                    return Err(format!(
                        "foreign server '{name}' disappeared while listing its metadata"
                    ));
                }
                Err(err) => return Err(format!("Failed to read foreign server '{name}': {err}")),
            }
        }
        print_result(
            &SQLResult::from_rows(
                vec!["server_name".into(), "fdw_type".into(), "options".into()],
                rows,
            ),
            out,
        );
        Ok(())
    }

    pub(super) fn cmd_list_graphs(&self, out: &mut impl Write) -> Result<(), String> {
        let names = self
            .engine
            .list_graphs()
            .map_err(|err| format!("Failed to read graphs: {err}"))?;
        if names.is_empty() {
            let _ = writeln!(out, "No named graphs.");
            return Ok(());
        }
        let mut rows = Vec::new();
        for name in names {
            let counts = match self.engine.graph_with(&name, |store| {
                let vertices = store.vertex_ids_in_graph(&name)?.len();
                let edges = store.edges_in_graph(&name)?.len();
                Ok::<_, uqa_graph::GraphStoreError>((vertices, edges))
            }) {
                Ok(Some(Ok(counts))) => counts,
                Ok(Some(Err(err))) => {
                    return Err(format!("Failed to read graph '{name}': {err}"));
                }
                Ok(None) => {
                    return Err(format!(
                        "graph '{name}' disappeared while listing its metadata"
                    ));
                }
                Err(err) => return Err(format!("Failed to read graph '{name}': {err}")),
            };
            rows.push(result_row(vec![
                ("graph_name", Value::Str(name)),
                ("vertices", usize_count_value(counts.0)),
                ("edges", usize_count_value(counts.1)),
            ]));
        }
        print_result(
            &SQLResult::from_rows(
                vec!["graph_name".into(), "vertices".into(), "edges".into()],
                rows,
            ),
            out,
        );
        Ok(())
    }

    pub(super) fn handle_migrate_python_db(
        &mut self,
        arg: &str,
        out: &mut impl Write,
    ) -> Result<(), String> {
        let parts = arg.split_whitespace().collect::<Vec<_>>();
        if parts.len() != 2 {
            return Err("usage: \\migrate-python-db <source> <destination>".into());
        }
        let source = Path::new(parts[0]);
        let destination = Path::new(parts[1]);
        let report = migrate_python_database(source, destination)
            .map_err(|err| format!("migration failed: {err}"))?;
        print_migration_report(&report, out)
            .map_err(|error| format!("write migration report failed: {error}"))?;
        let engine = Engine::open(destination)
            .map_err(|err| format!("open migrated database failed: {err}"))?;
        self.engine = engine;
        self.db_path = Some(destination.to_path_buf());
        self.db_key = None;
        self.location = destination.display().to_string();
        let _ = writeln!(out, "opened {}", self.location);
        Ok(())
    }
}
