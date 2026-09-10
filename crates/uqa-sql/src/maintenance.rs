//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Interpret VACUUM options and bind target relation metadata before storage maintenance.
use crate::{
    ast::{VacuumOption, VacuumOptionValue, VacuumStmt},
    SQLError,
};
use std::collections::BTreeSet;
pub struct VacuumOptions {
    flags: VacuumFlags,
}
pub struct ResolvedVacuumTarget {
    pub table: String,
    pub include_descendants: bool,
    pub columns: Vec<String>,
}
pub trait VacuumRelation {
    fn column_names(&self) -> BTreeSet<String>;
}
pub trait VacuumCatalog {
    fn resolve_relation_kind(&self, name: &str)
        -> Result<Option<(String, &'static str)>, SQLError>;
    fn require_table(&self, name: &str) -> Result<Box<dyn VacuumRelation + '_>, SQLError>;
}
pub trait VacuumPrivileges {
    fn ensure_maintain(&self, table: &str) -> Result<(), SQLError>;
}
#[derive(Clone, Copy, Default)]
struct VacuumFlags(u8);

impl VacuumFlags {
    const ANALYZE: u8 = 1 << 0;
    const FULL: u8 = 1 << 1;
    const DISABLE_PAGE_SKIPPING: u8 = 1 << 2;
    const PROCESS_TOAST: u8 = 1 << 3;
    const ONLY_DATABASE_STATS: u8 = 1 << 4;
    const ONLY_DATABASE_STATS_CONFLICT: u8 = 1 << 5;

    fn insert(&mut self, flag: u8, enabled: bool) {
        if enabled {
            self.0 |= flag;
        }
    }

    pub const fn contains(self, flag: u8) -> bool {
        self.0 & flag != 0
    }
}

impl VacuumOptions {
    pub const fn analyze(&self) -> bool {
        self.flags.contains(VacuumFlags::ANALYZE)
    }

    pub const fn full(&self) -> bool {
        self.flags.contains(VacuumFlags::FULL)
    }

    pub const fn disable_page_skipping(&self) -> bool {
        self.flags.contains(VacuumFlags::DISABLE_PAGE_SKIPPING)
    }

    pub const fn process_toast(&self) -> bool {
        self.flags.contains(VacuumFlags::PROCESS_TOAST)
    }

    pub const fn only_database_stats(&self) -> bool {
        self.flags.contains(VacuumFlags::ONLY_DATABASE_STATS)
    }

    pub const fn has_only_database_stats_conflict(&self) -> bool {
        self.flags
            .contains(VacuumFlags::ONLY_DATABASE_STATS_CONFLICT)
    }
}

fn vacuum_syntax_error(message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: "42601".into(),
        message: message.into(),
    }
}

fn invalid_buffer_usage_limit() -> SQLError {
    SQLError::Routine {
        sqlstate: "22023".into(),
        message: "BUFFER_USAGE_LIMIT option must be 0 or between 128 kB and 16777216 kB".into(),
    }
}

fn vacuum_feature_error(message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: "0A000".into(),
        message: message.into(),
    }
}

fn buffer_usage_limit_kib(value: &VacuumOptionValue) -> Result<u64, SQLError> {
    let (amount, multiplier) = match value {
        VacuumOptionValue::Integer(value) => {
            let amount = u64::try_from(*value).map_err(|_| invalid_buffer_usage_limit())?;
            return Ok(amount);
        }
        VacuumOptionValue::String(value) => {
            let value = value.trim();
            let split = value
                .find(|character: char| !(character.is_ascii_digit() || character == '.'))
                .unwrap_or(value.len());
            let amount = value[..split]
                .parse::<f64>()
                .map_err(|_| invalid_buffer_usage_limit())?;
            let unit = value[split..].trim().to_ascii_lowercase();
            let multiplier = match unit.as_str() {
                "" | "kb" => 1_f64,
                "mb" => 1024_f64,
                "gb" => 1024_f64 * 1024_f64,
                "tb" => 1024_f64 * 1024_f64 * 1024_f64,
                _ => return Err(invalid_buffer_usage_limit()),
            };
            (amount, multiplier)
        }
        VacuumOptionValue::Boolean(_) => return Err(invalid_buffer_usage_limit()),
    };
    let kib = amount * multiplier;
    if !kib.is_finite() || kib < 0.0 || kib.round() > u64::MAX as f64 {
        return Err(invalid_buffer_usage_limit());
    }
    Ok(kib.round() as u64)
}

fn boolean_option(option: &VacuumOption) -> Result<bool, SQLError> {
    let Some(value) = option.value.as_ref() else {
        return Ok(true);
    };
    match value {
        VacuumOptionValue::Boolean(value) => Ok(*value),
        VacuumOptionValue::String(value) => match value.to_ascii_lowercase().as_str() {
            "true" | "on" => Ok(true),
            "false" | "off" => Ok(false),
            _ => Err(vacuum_syntax_error(format!(
                "{} requires a Boolean value",
                option.name
            ))),
        },
        VacuumOptionValue::Integer(0) => Ok(false),
        VacuumOptionValue::Integer(1) => Ok(true),
        VacuumOptionValue::Integer(_) => Err(vacuum_syntax_error(format!(
            "{} requires a Boolean value",
            option.name
        ))),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves VACUUM option validation order"
)]
fn validate_options(options: &[VacuumOption]) -> Result<VacuumOptions, SQLError> {
    let mut analyze = false;
    let mut full = false;
    let mut parallel = 0_i64;
    let mut buffer_usage_limit_specified = false;
    let mut disable_page_skipping = false;
    let mut process_toast = true;
    let mut only_database_stats = false;
    let mut effective_options = BTreeSet::new();
    for option in options {
        match option.name.as_str() {
            "analyze" => {
                analyze = boolean_option(option)?;
                if analyze {
                    effective_options.insert(option.name.as_str());
                } else {
                    effective_options.remove(option.name.as_str());
                }
            }
            "full" => {
                full = boolean_option(option)?;
                if full {
                    effective_options.insert(option.name.as_str());
                } else {
                    effective_options.remove(option.name.as_str());
                }
            }
            "only_database_stats" => only_database_stats = boolean_option(option)?,
            "freeze" | "skip_locked" | "skip_database_stats" => {
                if boolean_option(option)? {
                    effective_options.insert(option.name.as_str());
                } else {
                    effective_options.remove(option.name.as_str());
                }
            }
            "disable_page_skipping" => {
                disable_page_skipping = boolean_option(option)?;
                if disable_page_skipping {
                    effective_options.insert(option.name.as_str());
                } else {
                    effective_options.remove(option.name.as_str());
                }
            }
            "process_toast" => process_toast = boolean_option(option)?,
            // PostgreSQL 18 permits these with ONLY_DATABASE_STATS; they are not part of the VACOPT conflict mask used by that check.
            "verbose" | "process_main" | "truncate" => {
                boolean_option(option)?;
            }
            "index_cleanup" => {
                if !matches!(
                    option.value.as_ref(),
                    Some(VacuumOptionValue::String(value)) if value.eq_ignore_ascii_case("auto")
                ) {
                    boolean_option(option)?;
                }
            }
            "parallel" => {
                let Some(VacuumOptionValue::Integer(workers)) = option.value.as_ref() else {
                    return Err(vacuum_syntax_error(
                        "parallel requires a non-negative integer value",
                    ));
                };
                if !(0..=1024).contains(workers) {
                    return Err(vacuum_syntax_error(
                        "parallel workers for vacuum must be between 0 and 1024",
                    ));
                }
                parallel = i64::from(*workers);
            }
            "buffer_usage_limit" => {
                buffer_usage_limit_specified = true;
                let kib = option
                    .value
                    .as_ref()
                    .ok_or_else(invalid_buffer_usage_limit)
                    .and_then(buffer_usage_limit_kib)?;
                if kib != 0 && !(128..=16_777_216).contains(&kib) {
                    return Err(invalid_buffer_usage_limit());
                }
            }
            name => {
                return Err(vacuum_syntax_error(format!(
                    "unrecognized VACUUM option \"{name}\""
                )));
            }
        }
    }
    if full && parallel > 0 {
        return Err(vacuum_feature_error(
            "VACUUM FULL cannot be performed in parallel",
        ));
    }
    if buffer_usage_limit_specified && full && !analyze {
        return Err(vacuum_feature_error(
            "BUFFER_USAGE_LIMIT cannot be specified for VACUUM FULL",
        ));
    }
    let mut flags = VacuumFlags::default();
    flags.insert(VacuumFlags::ANALYZE, analyze);
    flags.insert(VacuumFlags::FULL, full);
    flags.insert(VacuumFlags::DISABLE_PAGE_SKIPPING, disable_page_skipping);
    flags.insert(VacuumFlags::PROCESS_TOAST, process_toast);
    flags.insert(VacuumFlags::ONLY_DATABASE_STATS, only_database_stats);
    flags.insert(
        VacuumFlags::ONLY_DATABASE_STATS_CONFLICT,
        !effective_options.is_empty(),
    );
    Ok(VacuumOptions { flags })
}

pub fn analyze_vacuum(statement: &VacuumStmt) -> Result<VacuumOptions, SQLError> {
    let execution = validate_options(&statement.options)?;
    if !execution.analyze()
        && statement
            .targets
            .iter()
            .any(|target| !target.columns.is_empty())
    {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "ANALYZE option must be specified when a column list is provided".into(),
        });
    }
    if execution.full() && execution.disable_page_skipping() {
        return Err(vacuum_feature_error(
            "VACUUM option DISABLE_PAGE_SKIPPING cannot be used with FULL",
        ));
    }
    if execution.full() && !execution.process_toast() {
        return Err(vacuum_feature_error(
            "PROCESS_TOAST required with VACUUM FULL",
        ));
    }
    if execution.only_database_stats() && !statement.targets.is_empty() {
        return Err(vacuum_feature_error(
            "ONLY_DATABASE_STATS cannot be specified with a list of tables",
        ));
    }
    if execution.only_database_stats() && execution.has_only_database_stats_conflict() {
        return Err(vacuum_feature_error(
            "ONLY_DATABASE_STATS cannot be specified with other VACUUM options",
        ));
    }

    Ok(execution)
}
pub fn bind_vacuum_targets(
    catalog: &dyn VacuumCatalog,
    privileges: &dyn VacuumPrivileges,
    statement: &VacuumStmt,
) -> Result<Vec<ResolvedVacuumTarget>, SQLError> {
    let mut resolved_targets = Vec::with_capacity(statement.targets.len());
    for target in &statement.targets {
        if target
            .catalog
            .as_deref()
            .is_some_and(|catalog| catalog != "uqa")
        {
            let qualified = format!(
                "{}.{}",
                target.catalog.as_deref().expect("checked catalog"),
                target.table
            );
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: format!("cross-database references are not implemented: \"{qualified}\""),
            });
        }
        let canonical = match catalog.resolve_relation_kind(&target.table)? {
            Some((canonical, "table")) => canonical,
            Some(_) | None => return Err(SQLError::UnknownTable(target.table.clone())),
        };
        let table = catalog.require_table(&canonical)?;
        if !target.columns.is_empty() {
            let available = table.column_names();
            if let Some(column) = target
                .columns
                .iter()
                .find(|column| !available.contains(column.as_str()))
            {
                return Err(SQLError::Routine {
                    sqlstate: "42703".into(),
                    message: format!(
                        "column \"{column}\" of relation \"{}\" does not exist",
                        target.table
                    ),
                });
            }
        }
        privileges.ensure_maintain(&canonical)?;
        resolved_targets.push(ResolvedVacuumTarget {
            table: canonical,
            include_descendants: target.include_descendants,
            columns: target.columns.clone(),
        });
    }

    Ok(resolved_targets)
}
