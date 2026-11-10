// Once a tick, on the window's Game: what the world and Baritone look like
// goes to the engine, and what the engine answers — run the next step, end
// one, leave the world — is carried out here, through Baritone's own
// command surface. The engine (the runtime module) never touches the game.
// Copyright (c) 2026 hakergeniusz. MIT.
fn baritone_helper_tick(game: &mut crate::game::Game<'_>) {
    use crate::game::baritone_helper as helper;

    // The window is the Game that joined a server. The integrated server's
    // Game holds the world-open copy of the mods list and never drives the
    // queue (the same rule nohunger's gate follows).
    if game.remote.is_none() || !game.native_on(helper::ID) {
        return;
    }

    // What Baritone is doing. A temporary process in control means nothing:
    // it is Baritone's own business, not a command the player gave, so its
    // name is filtered out and the step keeps its own.
    let (baritone_active, proc, status) = crate::game::baritone::with(game, |b, _| {
        let described = b.describe().filter(|d| !d.2);
        let proc = described.as_ref().map(|d| d.0.to_owned());
        let status = described.map(|d| d.3);
        (b.procs.any_active() || b.core.pathing_busy(), proc, status)
    })
    .unwrap_or((false, None, None));

    // The watchdog's armed triggers, persisted in the mod's settings.
    let watchdog = helper::Watchdog {
        mobs_blocks: game
            .native_setting(helper::ID, "leave_mobs")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        night: game.native_setting(helper::ID, "leave_night").is_some_and(|v| v == "true"),
        health_below: game
            .native_setting(helper::ID, "leave_health")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.0),
        hunger_below: game
            .native_setting(helper::ID, "leave_hunger")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
    };

    // The player.
    let player = game.player.as_ref();
    let position = player.map(|p| p.position);
    let health = player.map_or(20.0, |p| p.health.health());
    let food = player.map_or(20, |p| p.health.food.food);

    // The inventory: the running count limit's item, and whether any slot
    // is open. Bare names ("diamond") match their namespaced ones.
    let item = helper::with_engine(|e| e.counted_item().map(str::to_owned));
    let mut counted: u32 = 0;
    let mut filled = 0;
    for stack in game.inventory.iter().flatten() {
        if stack.count > 0 {
            filled += 1;
            let name: &str = &stack.item;
            let bare = name.strip_prefix("minecraft:").unwrap_or(name);
            if item.as_deref() == Some(bare) {
                counted = counted.saturating_add(stack.count as u32);
            }
        }
    }
    let inventory_full = !game.inventory.is_empty() && filled == game.inventory.len();

    // A hostile mob within the watchdog's radius (read only when armed).
    let mut hostile_near = false;
    if watchdog.mobs_blocks > 0 {
        let reach = f64::from(watchdog.mobs_blocks);
        let reach = reach * reach;
        if let Some(p) = position {
            for m in game.mobs.iter() {
                if !m.alive() || !m.hostile() {
                    continue;
                }
                let (dx, dy, dz) =
                    (m.position.x - p.x, m.position.y - p.y, m.position.z - p.z);
                if dx * dx + dy * dy + dz * dz <= reach {
                    hostile_near = true;
                    break;
                }
            }
        }
    }

    // Night: the vanilla window of the day.
    let night = (12542..=23459).contains(&(game.world.day_time.rem_euclid(24000)));

    let snap = helper::Snapshot {
        in_world: player.is_some() && game.title.is_none(),
        alive: health > 0.0,
        baritone_active,
        counted,
        inventory_full,
        health,
        food,
        hostile_near,
        night,
        status,
        proc,
    };

    match helper::with_engine(|e| e.tick(&snap, &watchdog)) {
        helper::Action::Nothing => {}
        helper::Action::Run(line) => {
            // The next step of the queue, as if the player typed it.
            crate::game::baritone::with(game, |b, game| {
                crate::game::baritone_command::run(b, game, &line);
            });
        }
        helper::Action::Done { cancel, message } => {
            crate::game::baritone::with(game, |b, game| {
                if cancel {
                    // The limit (not Baritone) ended the step: its own goal
                    // is still running, so it must be told to stop.
                    b.cancel_everything(game);
                }
                b.core.say(game, &format!("Helper: {message}"));
            });
        }
        helper::Action::Say(message) => {
            crate::game::baritone::with(game, |b, game| {
                b.core.say(game, &format!("Helper: {message}"));
            });
        }
        helper::Action::Leave(reason) => {
            crate::game::baritone::with(game, |b, game| {
                b.cancel_everything(game);
                b.core.say_red(game, &format!("Helper: leaving — {reason}"));
            });
            // The world closes whether Baritone heard us or not.
            let _ = game.close_world();
        }
    }
}
