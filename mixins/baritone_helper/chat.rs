// A line typed in chat, before it is sent: `!q …` commands, eaten here so
// the server never sees them. A `#` line while a step runs is the player
// taking over with their own Baritone command — never cancelled here (one
// of the two hooks runs first, and cancelling would kill the player's
// command); Baritone's own hook usually eats the line anyway, and the tick
// notices the takeover by the process that is in control.
// Copyright (c) 2026 hakergeniusz. MIT.
fn baritone_helper_chat(game: &mut crate::game::Game<'_>, message: &str) -> std::ops::ControlFlow<()> {
    use crate::game::baritone_helper as helper;

    if game.remote.is_none() || !game.native_on(helper::ID) {
        return std::ops::ControlFlow::Continue(());
    }

    // The watchdog's armed triggers, as the settings hold them.
    let readback = |game: &crate::game::Game<'_>| {
        let night = game
            .native_setting(helper::ID, "leave_night")
            .is_some_and(|v| v == "true");
        format!(
            "leave: mobs {} chunks, night {}, health {}, hunger {}",
            game.native_setting(helper::ID, "leave_mobs").unwrap_or_else(|| "0".to_owned()),
            if night { "on" } else { "off" },
            game.native_setting(helper::ID, "leave_health").unwrap_or_else(|| "0".to_owned()),
            game.native_setting(helper::ID, "leave_hunger").unwrap_or_else(|| "0".to_owned()),
        )
    };

    if message.starts_with('#') {
        let note = helper::with_engine(|e| {
            e.skip().map(|note| {
                e.set_paused(true);
                note
            })
        });
        if let Some(note) = note {
            crate::game::baritone::with(game, |b, game| {
                b.core.say(game, &format!("Helper: {note} — resume with {} resume", helper::PREFIX));
            });
        }
        return std::ops::ControlFlow::Continue(());
    }

    let Some(rest) = message.strip_prefix(helper::PREFIX) else {
        return std::ops::ControlFlow::Continue(());
    };
    // `!queue` is not `!q` with a word glued on.
    if !(rest.is_empty() || rest.starts_with(char::is_whitespace)) {
        return std::ops::ControlFlow::Continue(());
    }

    let op = match helper::parse_chat(rest) {
        Ok(op) => op,
        Err(why) => {
            crate::game::baritone::with(game, |b, game| {
                b.core.say_red(game, &format!("Helper: {why}"));
            });
            return std::ops::ControlFlow::Break(());
        }
    };

    // Whether Baritone must stop before this tick ends (the step it runs
    // was dropped, so its goal would go on without a reason).
    let mut cancel = false;
    match op {
        helper::Op::Queue(entries) => {
            let n = entries.len();
            let pending = helper::with_engine(|e| e.queue_entries(entries));
            let s = if n == 1 { "" } else { "s" };
            crate::game::baritone::with(game, |b, game| {
                b.core.say(game, &format!("Helper: {n} step{s} queued — {pending} pending"));
            });
        }
        helper::Op::Skip => {
            let note = helper::with_engine(|e| e.skip());
            cancel = note.is_some();
            let note = note.unwrap_or_else(|| "nothing is running".to_owned());
            crate::game::baritone::with(game, |b, game| {
                b.core.say(game, &format!("Helper: {note}"));
            });
        }
        helper::Op::Clear => {
            let had = helper::with_engine(|e| e.clear());
            cancel = had;
            crate::game::baritone::with(game, |b, game| {
                b.core.say(game, "Helper: the queue is empty");
            });
        }
        helper::Op::Pause => {
            helper::with_engine(|e| e.set_paused(true));
            crate::game::baritone::with(game, |b, game| {
                b.core.say(game, "Helper: paused (the watchdog stays armed)");
            });
        }
        helper::Op::Resume => {
            helper::with_engine(|e| e.set_paused(false));
            crate::game::baritone::with(game, |b, game| {
                b.core.say(game, "Helper: resumed");
            });
        }
        helper::Op::Status => {
            let mut lines = helper::with_engine(|e| e.status_lines());
            let described = crate::game::baritone::with(game, |b, _| b.describe()).flatten();
            if let Some((name, _, _, display)) = described {
                lines.push(format!("baritone: {name} — {display}"));
            }
            lines.push(readback(game));
            for line in &lines {
                crate::game::baritone::with(game, |b, game| {
                    b.core.say(game, line);
                });
            }
        }
        helper::Op::Watchdog(op) => match op {
            helper::WatchOp::Show => {
                let line = readback(game);
                crate::game::baritone::with(game, |b, game| {
                    b.core.say(game, &format!("Helper: {line}"));
                });
            }
            helper::WatchOp::Mobs(chunks) => {
                game.native_set_setting(helper::ID, "leave_mobs", &chunks.to_string());
                let line = if chunks == 0 {
                    "leave on mobs: off".to_owned()
                } else {
                    format!("leave on mobs within {chunks} chunks")
                };
                crate::game::baritone::with(game, |b, game| {
                    b.core.say(game, &format!("Helper: {line}"));
                });
            }
            helper::WatchOp::Night(on) => {
                game.native_set_setting(helper::ID, "leave_night", if on { "true" } else { "false" });
                let line = if on { "leave on night" } else { "leave on night: off" };
                crate::game::baritone::with(game, |b, game| {
                    b.core.say(game, &format!("Helper: {line}"));
                });
            }
            helper::WatchOp::Health(hp) => {
                game.native_set_setting(helper::ID, "leave_health", &format!("{hp}"));
                let line = if hp == 0.0 {
                    "leave on health: off".to_owned()
                } else {
                    format!("leave on health below {hp}")
                };
                crate::game::baritone::with(game, |b, game| {
                    b.core.say(game, &format!("Helper: {line}"));
                });
            }
            helper::WatchOp::Hunger(food) => {
                game.native_set_setting(helper::ID, "leave_hunger", &food.to_string());
                let line = if food == 0 {
                    "leave on hunger: off".to_owned()
                } else {
                    format!("leave on hunger below {food}")
                };
                crate::game::baritone::with(game, |b, game| {
                    b.core.say(game, &format!("Helper: {line}"));
                });
            }
            helper::WatchOp::Off => {
                game.native_set_setting(helper::ID, "leave_mobs", "0");
                game.native_set_setting(helper::ID, "leave_night", "false");
                game.native_set_setting(helper::ID, "leave_health", "0");
                game.native_set_setting(helper::ID, "leave_hunger", "0");
                crate::game::baritone::with(game, |b, game| {
                    b.core.say(game, "Helper: the watchdog is off");
                });
            }
        },
        helper::Op::Help => {
            for line in helper::help_lines() {
                crate::game::baritone::with(game, |b, game| {
                    b.core.say(game, &line);
                });
            }
        }
    }
    if cancel {
        crate::game::baritone::with(game, |b, game| b.cancel_everything(game));
    }
    std::ops::ControlFlow::Break(())
}
