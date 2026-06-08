//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//
// Run with: cargo run -p uqa-engine --example doomql
//
// Optional environment:
//   DOOMQL_ROOT=/path/to/doomql
//   DOOMQL_PLAYERS=4
//   DOOMQL_TICKS=120
//   DOOMQL_RENDER_EVERY=10
//   DOOMQL_PROFILE_VIEWS=rays,visible_tiles,render_3d_frame

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use uqa_core::Value;
use uqa_engine::{Engine, SQLResult};

type AppResult<T> = Result<T, Box<dyn std::error::Error>>;

fn main() -> AppResult<()> {
    let root = doomql_root()?;
    let player_count = env_usize("DOOMQL_PLAYERS", 4).max(1);
    let ticks = env_usize("DOOMQL_TICKS", 120);
    let render_every = env_usize("DOOMQL_RENDER_EVERY", 10);
    let profile_views = env_list("DOOMQL_PROFILE_VIEWS");

    let engine = Engine::new();
    let setup_start = Instant::now();
    load_game(&engine, &root)?;
    let player_ids = seed_players(&engine, player_count)?;
    let setup_elapsed = setup_start.elapsed();

    let gameloop = read_sql(&root.join("gameloop.sql"))?;
    let mut tick_elapsed = Duration::ZERO;
    let mut render_elapsed = Duration::ZERO;
    let mut last_render = SQLResult::empty();
    let mut render_samples = 0usize;

    for tick in 0..ticks {
        apply_inputs(&engine, &player_ids, tick)?;

        let tick_start = Instant::now();
        run_sql(&engine, "gameloop.sql", &gameloop)?;
        tick_elapsed += tick_start.elapsed();

        if render_every > 0 && (tick + 1 == ticks || tick % render_every == render_every - 1) {
            let render_start = Instant::now();
            last_render = render_player(&engine, player_ids[0])?;
            render_elapsed += render_start.elapsed();
            render_samples += 1;
        }
    }

    let render_rows = value_as_i64(&last_render, "rows").unwrap_or_default();
    let render_chars = value_as_i64(&last_render, "chars").unwrap_or_default();
    let tick_avg = if ticks == 0 {
        0.0
    } else {
        tick_elapsed.as_secs_f64() * 1000.0 / ticks as f64
    };

    println!("doomql_root={}", root.display());
    println!("players={}", player_ids.len());
    println!("ticks={ticks}");
    println!("setup_ms={:.3}", setup_elapsed.as_secs_f64() * 1000.0);
    println!("tick_total_ms={:.3}", tick_elapsed.as_secs_f64() * 1000.0);
    println!("tick_avg_ms={tick_avg:.3}");
    println!(
        "render_total_ms={:.3}",
        render_elapsed.as_secs_f64() * 1000.0
    );
    println!("render_samples={render_samples}");
    println!("render_rows={render_rows}");
    println!("render_chars={render_chars}");
    profile_selected_views(&engine, player_ids[0], &profile_views)?;

    Ok(())
}

fn doomql_root() -> AppResult<PathBuf> {
    let path = env::var_os("DOOMQL_ROOT").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../doomql"),
        PathBuf::from,
    );
    let root = path.canonicalize().map_err(|err| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("DOOMQL root not found at {}: {err}", path.display()),
        )
    })?;
    Ok(root)
}

fn env_usize(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
}

fn env_list(name: &str) -> Vec<String> {
    env::var(name)
        .ok()
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn load_game(engine: &Engine, root: &Path) -> AppResult<()> {
    for relative in [
        "gamestate.sql",
        "renderer.sql",
        "data/map.sql",
        "data/player.sql",
        "data/slug.sql",
    ] {
        let path = root.join(relative);
        let sql = read_sql(&path)?;
        run_sql(engine, relative, &sql)?;
    }
    Ok(())
}

fn seed_players(engine: &Engine, count: usize) -> AppResult<Vec<i64>> {
    let spawn_points = [
        (4.0, 4.0, 0.0, "1"),
        (24.0, 4.0, 1.57, "2"),
        (48.0, 6.0, std::f64::consts::PI, "3"),
        (10.0, 18.0, 0.0, "4"),
        (30.0, 20.0, 1.57, "5"),
        (50.0, 22.0, std::f64::consts::PI, "6"),
    ];
    let mut ids = Vec::with_capacity(count);
    for index in 0..count {
        let (x, y, dir, icon) = spawn_points[index % spawn_points.len()];
        let result = run_sql(
            engine,
            "insert player mob",
            &format!(
                "INSERT INTO mobs(kind, x, y, dir, name, sprite_id, minimap_icon) \
                 VALUES ('player', {x}, {y}, {dir}, 'player{index}', 1, '{icon}') RETURNING id"
            ),
        )?;
        let id = value_as_i64(&result, "id").ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "player insert did not return id",
            )
        })?;
        run_sql(
            engine,
            "insert player state",
            &format!("INSERT INTO players(id) VALUES ({id})"),
        )?;
        run_sql(
            engine,
            "insert player input",
            &format!("INSERT INTO inputs(player_id, action) VALUES ({id}, '')"),
        )?;
        ids.push(id);
    }
    Ok(ids)
}

fn apply_inputs(engine: &Engine, player_ids: &[i64], tick: usize) -> AppResult<()> {
    const ACTIONS: [&str; 6] = ["w", "a", "d", "s", "x", ""];
    for (index, player_id) in player_ids.iter().enumerate() {
        let action = ACTIONS[(tick + index) % ACTIONS.len()];
        run_sql(
            engine,
            "update input",
            &format!("UPDATE inputs SET action = '{action}' WHERE player_id = {player_id}"),
        )?;
    }
    Ok(())
}

fn render_player(engine: &Engine, player_id: i64) -> AppResult<SQLResult> {
    run_sql(
        engine,
        "render player",
        &format!(
            "SELECT COUNT(*) AS rows, SUM(LENGTH(full_row)) AS chars \
             FROM screen WHERE player_id = {player_id}"
        ),
    )
}

fn profile_selected_views(engine: &Engine, player_id: i64, views: &[String]) -> AppResult<()> {
    for view in views {
        if !is_known_render_view(view) {
            return Err(Box::new(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unsupported DOOMQL view `{view}`"),
            )));
        }
        let started = Instant::now();
        let result = run_sql(
            engine,
            "profile view",
            &format!("SELECT COUNT(*) AS rows FROM {view} WHERE player_id = {player_id}"),
        )?;
        let elapsed = started.elapsed();
        let rows = value_as_i64(&result, "rows").unwrap_or_default();
        println!(
            "profile_view={view} rows={rows} ms={:.3}",
            elapsed.as_secs_f64() * 1000.0
        );
    }
    Ok(())
}

fn is_known_render_view(name: &str) -> bool {
    matches!(
        name,
        "rays" | "visible_tiles" | "render_3d_frame" | "game_view" | "minimap" | "screen"
    )
}

fn read_sql(path: &Path) -> AppResult<String> {
    fs::read_to_string(path).map_err(|err| {
        Box::new(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("failed to read {}: {err}", path.display()),
        )) as Box<dyn std::error::Error>
    })
}

fn run_sql(engine: &Engine, label: &str, sql: &str) -> AppResult<SQLResult> {
    engine.sql(sql, &[]).map_err(|err| {
        Box::new(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{label}: {err}"),
        )) as Box<dyn std::error::Error>
    })
}

fn value_as_i64(result: &SQLResult, column: &str) -> Option<i64> {
    match result.rows.first()?.get(column)? {
        Value::Int(value) => Some(*value),
        Value::Float(value) => Some(*value as i64),
        _ => None,
    }
}
