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

use std::collections::{BTreeMap, BTreeSet};
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
    let player_count = env_usize("DOOMQL_PLAYERS", 4)?.max(1);
    let ticks = env_usize("DOOMQL_TICKS", 120)?;
    let render_every = env_usize("DOOMQL_RENDER_EVERY", 10)?;
    let profile_views = env_list("DOOMQL_PROFILE_VIEWS")?;
    let mode = optional_env("DOOMQL_MODE")?.unwrap_or_else(|| "native".into());

    if mode == "sql" {
        run_sql_game(&root, player_count, ticks, render_every, &profile_views)
    } else if mode == "native" {
        run_native_game(&root, player_count, ticks, render_every, &profile_views)
    } else {
        Err(Box::new(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unsupported DOOMQL_MODE `{mode}`"),
        )))
    }
}

fn run_sql_game(
    root: &Path,
    player_count: usize,
    ticks: usize,
    render_every: usize,
    profile_views: &[String],
) -> AppResult<()> {
    let engine = Engine::new();
    let setup_start = Instant::now();
    load_game(&engine, root)?;
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

    let (render_rows, render_chars) = if render_samples == 0 {
        (0, 0)
    } else {
        (
            required_result_i64(&last_render, "rows")?,
            required_result_i64(&last_render, "chars")?,
        )
    };
    let tick_avg = if ticks == 0 {
        0.0
    } else {
        tick_elapsed.as_secs_f64() * 1000.0 / ticks as f64
    };

    println!("doomql_root={}", root.display());
    println!("mode=sql");
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
    profile_selected_views(&engine, player_ids[0], profile_views)?;

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

fn optional_env(name: &str) -> AppResult<Option<String>> {
    match env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(Box::new(error)),
    }
}

fn env_usize(name: &str, default: usize) -> AppResult<usize> {
    optional_env(name)?.map_or_else(
        || Ok(default),
        |value| {
            value.parse::<usize>().map_err(|error| {
                Box::new(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{name} must be a non-negative integer: {error}"),
                )) as Box<dyn std::error::Error>
            })
        },
    )
}

fn env_list(name: &str) -> AppResult<Vec<String>> {
    Ok(optional_env(name)?
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default())
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

#[derive(Clone)]
struct NativeSettings {
    fov: f64,
    step: f64,
    max_steps: usize,
    view_w: i32,
    view_h: i32,
    move_speed: f64,
    turn_speed: f64,
    ammo_max: i64,
}

#[derive(Clone)]
struct NativeMob {
    id: i64,
    kind: String,
    owner: Option<i64>,
    x: f64,
    y: f64,
    dir: f64,
    minimap_icon: String,
}

#[derive(Clone)]
struct NativePlayer {
    score: i64,
    hp: i64,
    ammo: i64,
}

struct NativeState {
    settings: NativeSettings,
    map: BTreeMap<(i32, i32), char>,
    respawns: Vec<(f64, f64)>,
    mobs: BTreeMap<i64, NativeMob>,
    players: BTreeMap<i64, NativePlayer>,
    next_mob_id: i64,
}

struct NativeRender {
    rows: usize,
    chars: usize,
}

fn run_native_game(
    root: &Path,
    player_count: usize,
    ticks: usize,
    render_every: usize,
    profile_views: &[String],
) -> AppResult<()> {
    let engine = Engine::new();
    let setup_start = Instant::now();
    load_game(&engine, root)?;
    let player_ids = seed_players(&engine, player_count)?;
    let mut state = load_native_state(&engine)?;
    let setup_elapsed = setup_start.elapsed();

    let mut tick_elapsed = Duration::ZERO;
    let mut render_elapsed = Duration::ZERO;
    let mut last_render = NativeRender { rows: 0, chars: 0 };
    let mut render_samples = 0usize;

    for tick in 0..ticks {
        let actions = native_actions(&player_ids, tick);
        let tick_start = Instant::now();
        native_tick(&mut state, &actions, tick);
        tick_elapsed += tick_start.elapsed();

        if render_every > 0 && (tick + 1 == ticks || tick % render_every == render_every - 1) {
            let render_start = Instant::now();
            last_render = native_screen(&state, player_ids[0]);
            render_elapsed += render_start.elapsed();
            render_samples += 1;
        }
    }

    let tick_avg = if ticks == 0 {
        0.0
    } else {
        tick_elapsed.as_secs_f64() * 1000.0 / ticks as f64
    };

    println!("doomql_root={}", root.display());
    println!("mode=native");
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
    println!("render_rows={}", last_render.rows);
    println!("render_chars={}", last_render.chars);
    profile_native_views(&state, player_ids[0], profile_views)?;

    Ok(())
}

fn load_native_state(engine: &Engine) -> AppResult<NativeState> {
    let settings_row = run_sql(
        engine,
        "native settings",
        "SELECT fov, step, max_steps, view_w, view_h FROM settings",
    )?;
    let config_row = run_sql(
        engine,
        "native config",
        "SELECT player_move_speed, player_turn_speed, ammo_max FROM config",
    )?;
    let settings = NativeSettings {
        fov: required_result_f64(&settings_row, "fov")?,
        step: required_result_f64(&settings_row, "step")?,
        max_steps: usize::try_from(required_result_i64(&settings_row, "max_steps")?)
            .map_err(|_| invalid_result_value("max_steps", "non-negative usize"))?,
        view_w: i32::try_from(required_result_i64(&settings_row, "view_w")?)
            .map_err(|_| invalid_result_value("view_w", "signed 32-bit integer"))?,
        view_h: i32::try_from(required_result_i64(&settings_row, "view_h")?)
            .map_err(|_| invalid_result_value("view_h", "signed 32-bit integer"))?,
        move_speed: required_result_f64(&config_row, "player_move_speed")?,
        turn_speed: required_result_f64(&config_row, "player_turn_speed")?,
        ammo_max: required_result_i64(&config_row, "ammo_max")?,
    };

    let map_rows = run_sql(engine, "native map", "SELECT x, y, tile FROM map")?;
    let mut map = BTreeMap::new();
    let mut respawns = Vec::new();
    for row in &map_rows.rows {
        let x = i32::try_from(required_row_i64(row, "x")?)
            .map_err(|_| invalid_result_value("x", "signed 32-bit integer"))?;
        let y = i32::try_from(required_row_i64(row, "y")?)
            .map_err(|_| invalid_result_value("y", "signed 32-bit integer"))?;
        let tile = required_row_char(row, "tile")?;
        if tile == 'R' {
            respawns.push((f64::from(x), f64::from(y)));
        }
        map.insert((x, y), tile);
    }
    if respawns.is_empty() {
        respawns.push((4.0, 4.0));
    }

    let mob_rows = run_sql(
        engine,
        "native mobs",
        "SELECT m.id, m.kind, m.owner, m.x, m.y, m.dir, m.minimap_icon,
                p.score, p.hp, p.ammo
         FROM mobs m LEFT JOIN players p ON p.id = m.id",
    )?;
    let mut mobs = BTreeMap::new();
    let mut players = BTreeMap::new();
    let mut next_mob_id = 1_i64;
    for row in &mob_rows.rows {
        let id = required_row_i64(row, "id")?;
        let following_id = id
            .checked_add(1)
            .ok_or_else(|| invalid_result_value("id", "incrementable integer"))?;
        next_mob_id = next_mob_id.max(following_id);
        mobs.insert(
            id,
            NativeMob {
                id,
                kind: required_row_string(row, "kind")?,
                owner: optional_row_i64(row, "owner")?,
                x: required_row_f64(row, "x")?,
                y: required_row_f64(row, "y")?,
                dir: required_row_f64(row, "dir")?,
                minimap_icon: required_row_char(row, "minimap_icon")?.to_string(),
            },
        );
        if !matches!(row.get("score"), Some(Value::Null) | None) {
            players.insert(
                id,
                NativePlayer {
                    score: required_row_i64(row, "score")?,
                    hp: required_row_i64(row, "hp")?,
                    ammo: required_row_i64(row, "ammo")?,
                },
            );
        }
    }

    Ok(NativeState {
        settings,
        map,
        respawns,
        mobs,
        players,
        next_mob_id,
    })
}

fn native_actions(player_ids: &[i64], tick: usize) -> BTreeMap<i64, &'static str> {
    const ACTIONS: [&str; 6] = ["w", "a", "d", "s", "x", ""];
    player_ids
        .iter()
        .enumerate()
        .map(|(index, player_id)| (*player_id, ACTIONS[(tick + index) % ACTIONS.len()]))
        .collect()
}

fn native_tick(state: &mut NativeState, actions: &BTreeMap<i64, &'static str>, tick: usize) {
    for (player_id, action) in actions {
        let Some(snapshot) = state.mobs.get(player_id).cloned() else {
            continue;
        };
        match *action {
            "w" | "s" => {
                let direction = if *action == "w" { 1.0 } else { -1.0 };
                let nx = snapshot.x + snapshot.dir.cos() * state.settings.move_speed * direction;
                let ny = snapshot.y + snapshot.dir.sin() * state.settings.move_speed * direction;
                if !native_wall_at(state, nx, ny) {
                    if let Some(mob) = state.mobs.get_mut(player_id) {
                        mob.x = nx;
                        mob.y = ny;
                    }
                }
            }
            "a" => {
                if let Some(mob) = state.mobs.get_mut(player_id) {
                    mob.dir -= state.settings.turn_speed;
                }
            }
            "d" => {
                if let Some(mob) = state.mobs.get_mut(player_id) {
                    mob.dir += state.settings.turn_speed;
                }
            }
            "x" => native_fire(state, *player_id),
            _ => {}
        }
    }

    for mob in state.mobs.values_mut().filter(|mob| mob.kind == "bullet") {
        mob.x += mob.dir.cos() * 0.5;
        mob.y += mob.dir.sin() * 0.5;
    }

    let mut remove = BTreeSet::new();
    for mob in state.mobs.values().filter(|mob| mob.kind == "bullet") {
        if native_wall_at(state, mob.x, mob.y) || !state.map.contains_key(&tile_pos(mob.x, mob.y)) {
            remove.insert(mob.id);
        }
    }

    let bullets: Vec<NativeMob> = state
        .mobs
        .values()
        .filter(|mob| mob.kind == "bullet" && !remove.contains(&mob.id))
        .cloned()
        .collect();
    let mut hits = Vec::new();
    for bullet in bullets {
        for player_id in state.players.keys() {
            if Some(*player_id) == bullet.owner {
                continue;
            }
            let Some(player_mob) = state.mobs.get(player_id) else {
                continue;
            };
            if tile_pos(player_mob.x, player_mob.y) == tile_pos(bullet.x, bullet.y) {
                hits.push((bullet.id, bullet.owner, *player_id));
            }
        }
    }
    for (bullet_id, owner_id, player_id) in hits {
        let killed = if let Some(player) = state.players.get_mut(&player_id) {
            player.hp -= 50;
            player.hp <= 0
        } else {
            false
        };
        if killed {
            if let Some(owner) = owner_id.and_then(|id| state.players.get_mut(&id)) {
                owner.score += 1;
            }
        }
        remove.insert(bullet_id);
    }
    for id in remove {
        state.mobs.remove(&id);
    }

    let dead: Vec<i64> = state
        .players
        .iter()
        .filter_map(|(id, player)| (player.hp <= 0).then_some(*id))
        .collect();
    for (offset, player_id) in dead.into_iter().enumerate() {
        let respawn = state.respawns[(tick + offset) % state.respawns.len()];
        if let Some(mob) = state.mobs.get_mut(&player_id) {
            mob.x = respawn.0;
            mob.y = respawn.1;
            mob.dir = 0.0;
        }
        if let Some(player) = state.players.get_mut(&player_id) {
            player.hp = 100;
            player.ammo = state.settings.ammo_max;
        }
    }

    if tick % 20 == 0 {
        for player in state.players.values_mut() {
            player.ammo = (player.ammo + 1).min(state.settings.ammo_max);
        }
    }
}

fn native_fire(state: &mut NativeState, player_id: i64) {
    let Some(player) = state.players.get_mut(&player_id) else {
        return;
    };
    if player.ammo <= 0 {
        return;
    }
    let Some(source) = state.mobs.get(&player_id).cloned() else {
        return;
    };
    player.ammo -= 1;
    let id = state.next_mob_id;
    state.next_mob_id += 1;
    state.mobs.insert(
        id,
        NativeMob {
            id,
            kind: "bullet".into(),
            owner: Some(player_id),
            x: source.x,
            y: source.y,
            dir: source.dir,
            minimap_icon: "*".into(),
        },
    );
}

fn native_screen(state: &NativeState, player_id: i64) -> NativeRender {
    let frame = native_render_3d_frame(state, player_id);
    let minimap = native_minimap(state, player_id);
    let hud = native_hud(state);
    let rows = frame.len().max(minimap.len()).max(hud.len());
    let mut chars = 0usize;
    for idx in 0..rows {
        let left = frame.get(idx).map_or("", String::as_str);
        let mid = minimap.get(idx).map_or("", String::as_str);
        let right = hud.get(idx).map_or("", String::as_str);
        chars += left.len() + 3 + mid.len() + 3 + right.len();
    }
    NativeRender { rows, chars }
}

fn native_render_3d_frame(state: &NativeState, player_id: i64) -> Vec<String> {
    let Some(player) = state.mobs.get(&player_id) else {
        return Vec::new();
    };
    let width = state.settings.view_w;
    let height = state.settings.view_h;
    let mut column_heights = Vec::new();
    let mut column_dists = Vec::new();
    for col in 0..=width {
        let angle = player.dir - state.settings.fov / 2.0
            + state.settings.fov * (f64::from(col) / f64::from((width - 1).max(1)));
        let hit = native_cast_ray(state, player.x, player.y, angle);
        let corrected = (hit.dist * (angle - player.dir).cos()).max(0.001);
        let wall_h = (f64::from(height) / corrected).round() as i32;
        column_heights.push(wall_h.clamp(0, height));
        column_dists.push(hit.dist);
    }

    let mut rows = Vec::new();
    rows.push(format!("+{}+", "-".repeat((width + 1) as usize)));
    for y in 0..=height {
        let mut row = String::with_capacity((width + 3) as usize);
        row.push('|');
        for col in 0..=width {
            let wall_h = column_heights[col as usize];
            let dist = column_dists[col as usize];
            let top = (height - wall_h) / 2;
            let bottom = (height + wall_h) / 2;
            let ch = if y < top {
                ' '
            } else if y >= bottom {
                '.'
            } else if dist < 2.5 {
                '#'
            } else if dist < 5.0 {
                '+'
            } else {
                '-'
            };
            row.push(ch);
        }
        row.push('|');
        rows.push(row);
    }
    rows.push(format!("+{}+", "-".repeat((width + 1) as usize)));
    rows
}

struct RayHit {
    dist: f64,
}

fn native_cast_ray(state: &NativeState, x: f64, y: f64, angle: f64) -> RayHit {
    let mut fx = x;
    let mut fy = y;
    for step in 1..=state.settings.max_steps {
        fx += angle.cos() * state.settings.step;
        fy += angle.sin() * state.settings.step;
        if native_wall_at(state, fx, fy) {
            return RayHit {
                dist: step as f64 * state.settings.step,
            };
        }
    }
    RayHit {
        dist: state.settings.max_steps as f64 * state.settings.step,
    }
}

fn native_minimap(state: &NativeState, viewer_id: i64) -> Vec<String> {
    let max_x = state.map.keys().map(|(x, _)| *x).max().unwrap_or(0);
    let max_y = state.map.keys().map(|(_, y)| *y).max().unwrap_or(0);
    let mut rows = Vec::new();
    rows.push(format!("+{}+", "-".repeat((max_x + 1) as usize)));
    for y in 0..=max_y {
        let mut row = String::with_capacity((max_x + 3) as usize);
        row.push('|');
        for x in 0..=max_x {
            let mut ch = match state.map.get(&(x, y)).copied().unwrap_or('#') {
                'R' => '.',
                tile => tile,
            };
            for mob in state.mobs.values() {
                if tile_pos(mob.x, mob.y) == (x, y) {
                    ch = if mob.id == viewer_id {
                        '@'
                    } else {
                        mob.minimap_icon
                            .chars()
                            .next()
                            .expect("native mob icons are validated while loading")
                    };
                }
            }
            row.push(ch);
        }
        row.push('|');
        rows.push(row);
    }
    rows.push(format!("+{}+", "-".repeat((max_x + 1) as usize)));
    rows
}

fn native_hud(state: &NativeState) -> Vec<String> {
    state
        .players
        .iter()
        .map(|(id, player)| {
            let name = state
                .mobs
                .get(id)
                .map_or("?", |mob| mob.minimap_icon.as_str());
            format!(
                "{id}: {name} score: {} HP: {} AMMO: {}",
                player.score, player.hp, player.ammo
            )
        })
        .collect()
}

fn profile_native_views(state: &NativeState, player_id: i64, views: &[String]) -> AppResult<()> {
    for view in views {
        if !is_known_render_view(view) {
            return Err(Box::new(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unsupported DOOMQL view `{view}`"),
            )));
        }
        let started = Instant::now();
        let rows = match view.as_str() {
            "rays" | "visible_tiles" => native_visible_tiles(state, player_id).len(),
            "render_3d_frame" | "game_view" => native_render_3d_frame(state, player_id).len(),
            "minimap" => native_minimap(state, player_id).len(),
            "screen" => native_screen(state, player_id).rows,
            _ => 0,
        };
        let elapsed = started.elapsed();
        println!(
            "profile_view={view} rows={rows} ms={:.3}",
            elapsed.as_secs_f64() * 1000.0
        );
    }
    Ok(())
}

fn native_visible_tiles(state: &NativeState, player_id: i64) -> BTreeSet<(i32, i32)> {
    let Some(player) = state.mobs.get(&player_id) else {
        return BTreeSet::new();
    };
    let mut out = BTreeSet::new();
    for col in 0..=state.settings.view_w {
        let angle = player.dir - state.settings.fov / 2.0
            + state.settings.fov * (f64::from(col) / f64::from((state.settings.view_w - 1).max(1)));
        let mut fx = player.x;
        let mut fy = player.y;
        for _ in 0..state.settings.max_steps {
            fx += angle.cos() * state.settings.step;
            fy += angle.sin() * state.settings.step;
            let pos = tile_pos(fx, fy);
            out.insert(pos);
            if native_wall_at(state, fx, fy) {
                break;
            }
        }
    }
    out
}

fn native_wall_at(state: &NativeState, x: f64, y: f64) -> bool {
    state
        .map
        .get(&tile_pos(x, y))
        .is_none_or(|tile| *tile == '#')
}

fn tile_pos(x: f64, y: f64) -> (i32, i32) {
    (x.floor() as i32, y.floor() as i32)
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
        let rows = required_result_i64(&result, "rows")?;
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

fn value_as_f64(result: &SQLResult, column: &str) -> Option<f64> {
    result.rows.first()?.get(column).and_then(value_to_f64)
}

fn required_result_i64(result: &SQLResult, column: &str) -> io::Result<i64> {
    value_as_i64(result, column).ok_or_else(|| invalid_result_value(column, "integer"))
}

fn required_result_f64(result: &SQLResult, column: &str) -> io::Result<f64> {
    value_as_f64(result, column).ok_or_else(|| invalid_result_value(column, "finite number"))
}

fn required_row_i64(row: &BTreeMap<String, Value>, column: &str) -> io::Result<i64> {
    row.get(column)
        .and_then(value_to_i64)
        .ok_or_else(|| invalid_result_value(column, "integer"))
}

fn optional_row_i64(row: &BTreeMap<String, Value>, column: &str) -> io::Result<Option<i64>> {
    match row.get(column) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value_to_i64(value)
            .map(Some)
            .ok_or_else(|| invalid_result_value(column, "nullable integer")),
    }
}

fn required_row_f64(row: &BTreeMap<String, Value>, column: &str) -> io::Result<f64> {
    row.get(column)
        .and_then(value_to_f64)
        .ok_or_else(|| invalid_result_value(column, "finite number"))
}

fn required_row_string(row: &BTreeMap<String, Value>, column: &str) -> io::Result<String> {
    match row.get(column) {
        Some(Value::Str(value)) => Ok(value.clone()),
        Some(Value::Int(value)) => Ok(value.to_string()),
        Some(Value::Float(value)) if value.is_finite() => Ok(value.to_string()),
        Some(Value::Bool(value)) => Ok(value.to_string()),
        _ => Err(invalid_result_value(column, "non-null scalar string")),
    }
}

fn required_row_char(row: &BTreeMap<String, Value>, column: &str) -> io::Result<char> {
    let value = required_row_string(row, column)?;
    let mut chars = value.chars();
    let first = chars
        .next()
        .ok_or_else(|| invalid_result_value(column, "single character"))?;
    if chars.next().is_some() {
        return Err(invalid_result_value(column, "single character"));
    }
    Ok(first)
}

fn invalid_result_value(column: &str, expected: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("SQL result column `{column}` is missing or is not a {expected}"),
    )
}

fn value_to_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Int(value) => Some(*value),
        Value::Float(value)
            if value.is_finite()
                && value.fract() == 0.0
                && *value >= i64::MIN as f64
                && *value < -(i64::MIN as f64) =>
        {
            Some(*value as i64)
        }
        _ => None,
    }
}

fn value_to_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Int(value) => Some(*value as f64),
        Value::Float(value) if value.is_finite() => Some(*value),
        _ => None,
    }
}
