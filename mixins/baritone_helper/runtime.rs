// Horizon Baritone Helper: stacked Baritone actions (do Y after doing X),
// limits that stop an action once its goal is met (mine 64 diamonds and
// quit), and a watchdog that leaves the world when danger shows up (a
// hostile mob within the radius, night, low health, low hunger).
//
// Copyright (c) 2026 hakergeniusz. MIT — an independent helper written for
// the Baritone native mod (luhnox's port of leijurv's Baritone); it drives
// Baritone through Baritone's own commands and copies nothing from it.
//
// This file is the mod's brain and its Cargo library: the queue engine, the
// limits, the watchdog policy and the `!q` chat grammar, all in plain
// standard library types so the whole logic runs and is tested outside the
// game. The game-side glue (reading the world, talking to Baritone) lives
// in the injected helpers; it feeds this engine a Snapshot each tick and
// carries out the Action the engine answers with — the engine never touches
// the game.

use std::collections::VecDeque;

/// The mod's id: its row in Options > Mods, its settings folder, and the
/// second word of every command (`!q …`).
pub const ID: &str = "baritone_helper";
pub const VERSION: &str = "0.1.0";
/// The chat prefix. Baritone's is `#`; this one starts with `!` so a line
/// can never mean both, whichever hook runs first.
pub const PREFIX: &str = "!q";
/// The mod's row in Options > Mods.
pub const DESCRIPTION: &str = "Queue Baritone steps (do Y after X), stop an action once its \
goal is met (mine 64 diamonds), and leave the world when danger shows up (a hostile mob \
near, night, low health or hunger). Type !q help in chat. Needs the Baritone mod.";

/// What stops the current action before Baritone finishes it by itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Limit {
    /// Run until Baritone is done (it walks to the goal, mines the count it
    /// was given, and so on).
    None,
    /// Stop once the inventory holds this many more of the item than it did
    /// when the action started — "mine 64 diamonds" counts diamonds, not
    /// ore blocks (fortune and deepslate variants make the two different).
    Count { item: String, n: u32 },
    /// Stop once no inventory slot is empty.
    Full,
    /// Stop after this many ticks.
    Ticks { t: u32 },
}

/// One queued step: a Baritone command line (its own prefix already off)
/// and the limit that ends it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub command: String,
    pub limit: Limit,
}

/// What one tick of the game looks like to the engine. The glue builds it
/// from the game and from Baritone; none of it outlives the tick.
pub struct Snapshot {
    /// The player is in a world (the engine forgets everything the moment
    /// this goes false — a new world starts with an empty queue).
    pub in_world: bool,
    /// The player is alive; the watchdog holds its tongue on the death
    /// screen (dying is its own reason to leave, not a surprise).
    pub alive: bool,
    /// Baritone is walking, mining, farming — something of its own is on.
    pub baritone_active: bool,
    /// How many of the running limit's item the inventory holds (0 when no
    /// count limit runs).
    pub counted: u32,
    /// No inventory slot is empty.
    pub inventory_full: bool,
    /// The player's health, half-hearts (a full bar is 20).
    pub health: f32,
    /// The player's food level (a full bar is 20).
    pub food: i32,
    /// A hostile mob stands within the watchdog's radius.
    pub hostile_near: bool,
    /// It is night (the vanilla window, 12542–23459 of the day).
    pub night: bool,
    /// What Baritone says it is doing, for the completion message.
    pub status: Option<String>,
    /// The name of the process in control of Baritone — `None` when it is
    /// idle, and also when that process is one of Baritone's own temporary
    /// helpers (the glue filters those out, they are its business). A step
    /// that watched one name and then sees another was interrupted: the
    /// player typed a Baritone command of their own.
    pub proc: Option<String>,
}

/// The watchdog's triggers, all off by default; `!q leave …` sets them and
/// the game persists them with the mod's own settings.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Watchdog {
    /// Leave when a hostile mob stands within this many blocks (0 = off).
    pub mobs_blocks: u32,
    /// Leave when night falls.
    pub night: bool,
    /// Leave when health falls below this (0 = off).
    pub health_below: f32,
    /// Leave when food falls below this (0 = off).
    pub hunger_below: i32,
}

impl Watchdog {
    /// Whether any trigger is armed.
    pub fn armed(&self) -> bool {
        self.mobs_blocks > 0 || self.night || self.health_below > 0.0 || self.hunger_below > 0
    }

    /// The reason to leave this tick, if any trigger fires.
    fn fire(&self, snap: &Snapshot) -> Option<String> {
        if !snap.alive {
            return None;
        }
        if self.mobs_blocks > 0 && snap.hostile_near {
            return Some(format!(
                "a hostile mob came within {} blocks",
                self.mobs_blocks
            ));
        }
        if self.night && snap.night {
            return Some("night fell".to_owned());
        }
        if self.health_below > 0.0 && snap.health < self.health_below {
            return Some(format!("health fell below {:.0}", self.health_below));
        }
        if self.hunger_below > 0 && snap.food < self.hunger_below {
            return Some(format!("hunger fell below {}", self.hunger_below));
        }
        None
    }
}

/// What the glue should do about this tick.
#[derive(Debug, PartialEq)]
pub enum Action {
    Nothing,
    /// Hand Baritone this command line.
    Run(String),
    /// The step ended: cancel Baritone's work when the limit (not Baritone)
    /// ended it, and say why.
    Done {
        cancel: bool,
        message: String,
    },
    /// The watchdog fired: cancel Baritone and leave the world.
    Leave(String),
    /// Just say something (statuses, queue notes).
    Say(String),
}

/// The step that is running.
struct Current {
    entry: Entry,
    /// The counted item's inventory size when the step started (a count
    /// limit measures what the action gathered, not what was already there).
    started: u32,
    elapsed: u32,
    /// Consecutive ticks Baritone was idle: a command that never started
    /// (an unknown block name, an unreachable goal) must not wedge the
    /// queue — three idle ticks and the step is let go.
    idle: u32,
    status: Option<String>,
    /// The process the step's command put in control, once Baritone showed
    /// one. A different non-temporary name later means the player took over.
    proc: Option<String>,
}

impl Current {
    fn starting(entry: Entry, counted: u32) -> Self {
        Self {
            entry,
            started: counted,
            elapsed: 0,
            idle: 0,
            status: None,
            proc: None,
        }
    }
}

/// The queue engine, one per thread (the window's Game drives it; the
/// integrated server's never sees it).
#[derive(Default)]
pub struct Engine {
    queue: VecDeque<Entry>,
    current: Option<Current>,
    paused: bool,
    /// The watchdog fired and the glue is leaving: stay quiet until the
    /// world is actually gone, instead of asking every tick.
    leaving: Option<String>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            in_world: true,
            alive: true,
            baritone_active: false,
            counted: 0,
            inventory_full: false,
            health: 20.0,
            food: 20,
            hostile_near: false,
            night: false,
            status: None,
            proc: None,
        }
    }
}

impl Engine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget everything: the world closed (or a reset was asked for).
    pub fn reset(&mut self) {
        self.queue.clear();
        self.current = None;
        self.paused = false;
        self.leaving = None;
    }

    /// The item a running count limit watches, if there is one: the glue
    /// counts the inventory for it before the tick.
    pub fn counted_item(&self) -> Option<&str> {
        let limit = &self.current.as_ref()?.entry.limit;
        match limit {
            Limit::Count { item, .. } => Some(item),
            _ => None,
        }
    }

    /// One tick of the game.
    pub fn tick(&mut self, snap: &Snapshot, watchdog: &Watchdog) -> Action {
        if !snap.in_world {
            self.reset();
            return Action::Nothing;
        }
        if let Some(reason) = &self.leaving {
            let _ = reason;
            return Action::Nothing;
        }
        if let Some(reason) = watchdog.fire(snap) {
            self.leaving = Some(reason.clone());
            return Action::Leave(reason);
        }
        if self.paused {
            return Action::Nothing;
        }
        if let Some(current) = self.current.as_mut() {
            current.elapsed += 1;
            let mut takeover = false;
            if snap.baritone_active {
                current.idle = 0;
                if snap.status.is_some() {
                    current.status = snap.status.clone();
                }
                if current.proc.is_none() {
                    current.proc = snap.proc.clone();
                } else if snap.proc.is_some() && snap.proc != current.proc {
                    // A different process is in control: the player typed a
                    // Baritone command of their own and it replaced the step's.
                    takeover = true;
                }
            } else {
                current.idle += 1;
            }
            let (met, why) = match &current.entry.limit {
                Limit::None => (false, String::new()),
                Limit::Count { n, .. } => {
                    let got = snap.counted.saturating_sub(current.started);
                    (got >= *n, format!("collected {got} of {n}"))
                }
                Limit::Full => (snap.inventory_full, "the inventory filled".to_owned()),
                Limit::Ticks { t } => (current.elapsed >= *t, "time up".to_owned()),
            };
            if met {
                let current = self.current.take().unwrap();
                let message = finish(&current.entry, &why, self.queue.len());
                return Action::Done {
                    cancel: true,
                    message,
                };
            }
            if takeover {
                let current = self.current.take().unwrap();
                self.paused = true;
                let message = format!(
                    "{} — the player took over — the queue is paused, resume with {} resume",
                    current.entry.command, PREFIX
                );
                return Action::Done {
                    cancel: false,
                    message,
                };
            }
            if current.idle >= 3 && current.elapsed >= 2 {
                let current = self.current.take().unwrap();
                let why = current
                    .status
                    .unwrap_or_else(|| "baritone went idle".to_owned());
                let message = finish(&current.entry, &why, self.queue.len());
                return Action::Done {
                    cancel: false,
                    message,
                };
            }
            return Action::Nothing;
        }
        // Baritone is busy with something of the player's own: the queue
        // waits rather than yanking the goal out from under it.
        if snap.baritone_active {
            return Action::Nothing;
        }
        match self.queue.pop_front() {
            Some(entry) => {
                self.current = Some(Current::starting(entry.clone(), snap.counted));
                Action::Run(entry.command)
            }
            None => Action::Nothing,
        }
    }

    /// `!q …` with steps: queue them. Returns how many steps are pending.
    pub fn queue_entries(&mut self, entries: Vec<Entry>) -> usize {
        self.queue.extend(entries);
        self.queue.len()
    }

    /// `!q skip`: the running step is dropped now; the glue cancels
    /// Baritone. Some(message) when a step was actually running.
    pub fn skip(&mut self) -> Option<String> {
        let current = self.current.take()?;
        let message = format!("{} — skipped", current.entry.command);
        Some(queued_note(
            message,
            self.queue.is_empty(),
            self.queue.len(),
        ))
    }

    /// `!q clear`: the running step and the queue are dropped; the glue
    /// cancels Baritone when a step was running.
    pub fn clear(&mut self) -> bool {
        self.leaving = None;
        let had_current = self.current.take().is_some();
        let had_queue = !self.queue.is_empty();
        self.queue.clear();
        had_current || had_queue
    }

    /// `!q pause` / `!q resume`; the watchdog stays armed while paused.
    pub fn set_paused(&mut self, on: bool) {
        self.paused = on;
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// `!q status`: what the engine holds, for the glue to dress up.
    pub fn status_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if self.paused {
            lines.push("paused (the watchdog stays armed)".to_owned());
        }
        if let Some(current) = &self.current {
            let mut line = format!("running: {}", current.entry.command);
            let limit = match &current.entry.limit {
                Limit::None => String::new(),
                Limit::Count { item, n } => format!(" — until {n} {}", item),
                Limit::Full => " — until the inventory fills".to_owned(),
                Limit::Ticks { t } => format!(" — for {}s", t / 20),
            };
            line.push_str(&limit);
            lines.push(line);
        }
        for (i, entry) in self.queue.iter().enumerate() {
            lines.push(format!("queued {}: {}", i + 1, entry.command));
        }
        if lines.is_empty() {
            lines.push("the queue is empty".to_owned());
        }
        lines
    }
}

/// The message a finished step gets, with the queue's tail noted.
fn finish(entry: &Entry, why: &str, pending: usize) -> String {
    queued_note(
        format!("{} — {}", entry.command, why),
        pending == 0,
        pending,
    )
}

fn queued_note(message: String, queue_empty: bool, pending: usize) -> String {
    if queue_empty {
        format!("{message} — queue finished")
    } else {
        format!("{message} — {pending} queued")
    }
}

/// What `!q …` asks for.
#[derive(Debug)]
pub enum Op {
    Queue(Vec<Entry>),
    Skip,
    Clear,
    Pause,
    Resume,
    Status,
    Watchdog(WatchOp),
    Help,
}

/// The watchdog half of `!q leave …`; the glue persists each change in the
/// mod's settings and shows the current values for [`WatchOp::Show`].
#[derive(Debug)]
pub enum WatchOp {
    Show,
    Mobs(u32),
    Night(bool),
    Health(f32),
    Hunger(i32),
    Off,
}

/// The `!q` grammar, after the prefix: `[steps…]`, or a subcommand.
/// Steps are Baritone command lines joined by ` then `, each optionally
/// ended by ` until <n> <item>`, ` until full` or ` for <seconds>[s|m]`.
pub fn parse_chat(line: &str) -> Result<Op, String> {
    let line = line.trim();
    let (word, rest) = match line.find(char::is_whitespace) {
        Some(i) => (&line[..i], line[i..].trim_start()),
        None => (line, ""),
    };
    match word {
        "" | "status" | "queue" => Ok(Op::Status),
        "help" | "?" => Ok(Op::Help),
        "skip" => Ok(Op::Skip),
        "clear" => Ok(Op::Clear),
        "pause" => Ok(Op::Pause),
        "resume" => Ok(Op::Resume),
        "leave" => parse_leave(rest),
        // A stack can start with no step, but not with a dangling `then`.
        "then" => Err("a step between `then`s is empty".to_owned()),
        // Anything else is steps: `!q goto home then mine 64 diamond_ore`.
        _ => parse_entries(line).map(Op::Queue),
    }
}

fn parse_leave(rest: &str) -> Result<Op, String> {
    let (word, rest) = match rest.find(char::is_whitespace) {
        Some(i) => (&rest[..i], rest[i..].trim_start()),
        None => (rest, ""),
    };
    match word {
        "" => Ok(Op::Watchdog(WatchOp::Show)),
        "off" => Ok(Op::Watchdog(WatchOp::Off)),
        "night" => match rest {
            "on" | "true" => Ok(Op::Watchdog(WatchOp::Night(true))),
            "off" | "false" => Ok(Op::Watchdog(WatchOp::Night(false))),
            _ => Err("usage: !q leave night on|off".to_owned()),
        },
        "mobs" => rest
            .parse::<u32>()
            .map(|chunks| Op::Watchdog(WatchOp::Mobs(chunks)))
            .map_err(|_| "usage: !q leave mobs <chunks> (0 turns it off)".to_owned()),
        "health" => rest
            .parse::<f32>()
            .map(|hp| Op::Watchdog(WatchOp::Health(hp)))
            .map_err(|_| "usage: !q leave health <half-hearts> (0 turns it off)".to_owned()),
        "hunger" => rest
            .parse::<i32>()
            .map(|food| Op::Watchdog(WatchOp::Hunger(food)))
            .map_err(|_| "usage: !q leave hunger <food points> (0 turns it off)".to_owned()),
        _ => Err(
            "usage: !q leave [mobs <chunks>|night on|off|health <hp>|hunger <food>|off]".to_owned(),
        ),
    }
}

fn parse_entries(line: &str) -> Result<Vec<Entry>, String> {
    let mut entries = Vec::new();
    for segment in line.split(" then ") {
        let segment = segment.trim();
        if segment.is_empty() {
            return Err("a step between `then`s is empty".to_owned());
        }
        // A dangling `then` at the end of the line ("mine dirt then"): the
        // splitter only sees a `then` followed by a space, so an end-of-line
        // one is caught here instead. No Baritone command ends in the word.
        if segment
            .strip_suffix("then")
            .is_some_and(|head| head.is_empty() || head.ends_with(' '))
        {
            return Err("a step between `then`s is empty".to_owned());
        }
        let (command, limit) = limit_of(segment)?;
        entries.push(Entry { command, limit });
    }
    Ok(entries)
}

/// The limit at the end of a step, if its words end in one. The last
/// ` until …` / ` for …` wins, so a block name that contains the word
/// `until` survives unless a limit really follows it.
fn limit_of(segment: &str) -> Result<(String, Limit), String> {
    let mut limit = Limit::None;
    let mut command = segment.to_owned();
    if let Some((head, tail)) = split_last(&command, " until ") {
        let tail = tail.trim();
        limit = if tail == "full" {
            Limit::Full
        } else {
            let mut words = tail.split_whitespace();
            let n = words
                .next()
                .ok_or_else(|| "usage: … until <n> <item>".to_owned())?
                .parse::<u32>()
                .map_err(|_| format!("`{tail}`: the count must be a number"))?;
            let item = words.collect::<Vec<_>>().join(" ");
            if item.is_empty() {
                return Err(format!("`{tail}`: name the item to count"));
            }
            Limit::Count { item, n }
        };
        command = head.to_owned();
    } else if let Some((head, tail)) = split_last(&command, " for ") {
        let ticks = parse_duration(tail.trim())
            .ok_or_else(|| format!("`{tail}`: usage: for <seconds>[s|m]"))?;
        limit = Limit::Ticks { t: ticks };
        command = head.to_owned();
    }
    let command = command.trim().to_owned();
    if command.is_empty() {
        return Err("a step has no command".to_owned());
    }
    Ok((command, limit))
}

/// The last `separator` in `text`: everything before it and everything
/// after it, or None when it does not appear.
fn split_last<'a>(text: &'a str, separator: &str) -> Option<(&'a str, &'a str)> {
    let i = text.rfind(separator)?;
    Some((&text[..i], &text[i + separator.len()..]))
}

/// `30`, `30s` or `5m` — ticks.
fn parse_duration(text: &str) -> Option<u32> {
    let (value, unit) = match text.strip_suffix('s') {
        Some(rest) => (rest, 20u32),
        None => match text.strip_suffix('m') {
            Some(rest) => (rest, 1200),
            None => (text, 20),
        },
    };
    let seconds = value.parse::<u32>().ok()?;
    seconds.checked_mul(unit)
}

// The engine, one per thread. Only the window's Game reaches it.
thread_local! {
    static ENGINE: std::cell::RefCell<Engine> = std::cell::RefCell::new(Engine::new());
}

/// Run `f` with this thread's engine. The glue calls this once a tick and
/// once per chat command; the calls never nest.
pub fn with_engine<R>(f: impl FnOnce(&mut Engine) -> R) -> R {
    ENGINE.with_borrow_mut(f)
}

/// The help lines `!q help` gets, one per chat line.
pub fn help_lines() -> Vec<&'static str> {
    vec![
        "Baritone Helper — queue Baritone steps, stop them at a goal, leave on danger.",
        "!q <step> [then <step>…] — queue Baritone commands; each may end with",
        "  until <n> <item> — stop once the inventory holds n more of the item,",
        "  until full — stop once no slot is empty,",
        "  for <seconds>[s|m] — stop after that long.",
        "!q status | skip | clear | pause | resume",
        "!q leave — the auto-leave watchdog: leave mobs <chunks>, night on|off,",
        "  health <half-hearts>, hunger <food>, off (0 turns one trigger off).",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(line: &str) -> Vec<Entry> {
        match parse_chat(line).unwrap() {
            Op::Queue(entries) => entries,
            other => panic!("expected steps, got {other:?}"),
        }
    }

    /// A stacked line becomes one entry per step, each with its own limit,
    /// and the limits come off the Baritone command.
    #[test]
    fn a_line_stacks_steps_with_their_limits() {
        let got =
            entries("goto 100 64 -200 then mine 64 diamond_ore until 64 diamond then goto home");
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].command, "goto 100 64 -200");
        assert_eq!(got[0].limit, Limit::None);
        assert_eq!(got[1].command, "mine 64 diamond_ore");
        assert_eq!(
            got[1].limit,
            Limit::Count {
                item: "diamond".to_owned(),
                n: 64
            }
        );
        assert_eq!(got[2].command, "goto home");
        assert_eq!(got[2].limit, Limit::None);
    }

    /// The other two limits parse, the plain form needs no `run`, and a
    /// step without a command is an error.
    #[test]
    fn limits_and_the_bare_form_parse() {
        let got = entries("farm until full");
        assert_eq!(got[0].command, "farm");
        assert_eq!(got[0].limit, Limit::Full);

        let got = entries("follow steve for 90s");
        assert_eq!(got[0].limit, Limit::Ticks { t: 1800 });

        let got = entries("explore for 5m");
        assert_eq!(got[0].limit, Limit::Ticks { t: 6000 });

        assert!(parse_chat("then mine dirt").is_err());
        assert!(parse_chat("mine dirt then").is_err());
    }

    /// The subcommands route: a stack goes to Queue, the rest to their ops.
    #[test]
    fn subcommands_route() {
        assert!(matches!(parse_chat("status"), Ok(Op::Status)));
        assert!(matches!(parse_chat(""), Ok(Op::Status)));
        assert!(matches!(parse_chat("skip"), Ok(Op::Skip)));
        assert!(matches!(parse_chat("clear"), Ok(Op::Clear)));
        assert!(matches!(parse_chat("pause"), Ok(Op::Pause)));
        assert!(matches!(parse_chat("resume"), Ok(Op::Resume)));
        assert!(matches!(parse_chat("help"), Ok(Op::Help)));
        assert!(matches!(
            parse_chat("leave"),
            Ok(Op::Watchdog(WatchOp::Show))
        ));
    }

    /// The watchdog grammar: numbers, the on/off switches, off.
    #[test]
    fn the_watchdog_grammar_parses() {
        match parse_chat("leave mobs 16").unwrap() {
            Op::Watchdog(WatchOp::Mobs(chunks)) => assert_eq!(chunks, 16),
            other => panic!("{other:?}"),
        }
        match parse_chat("leave night on").unwrap() {
            Op::Watchdog(WatchOp::Night(on)) => assert!(on),
            other => panic!("{other:?}"),
        }
        match parse_chat("leave health 6").unwrap() {
            Op::Watchdog(WatchOp::Health(hp)) => assert_eq!(hp, 6.0),
            other => panic!("{other:?}"),
        }
        match parse_chat("leave hunger 8").unwrap() {
            Op::Watchdog(WatchOp::Hunger(food)) => assert_eq!(food, 8),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            parse_chat("leave off"),
            Ok(Op::Watchdog(WatchOp::Off))
        ));
        assert!(parse_chat("leave what").is_err());
    }

    /// The stack runs: the first step is submitted, the next only once
    /// Baritone goes idle, and the tail notes the queue.
    #[test]
    fn the_stack_runs_one_step_at_a_time() {
        let mut engine = Engine::new();
        let mut pending = entries("goto 0 64 0 then mine 64 diamond_ore");
        engine.queue_entries(std::mem::take(&mut pending));

        // First tick: the first step goes to Baritone.
        let action = engine.tick(&Snapshot::default(), &Watchdog::default());
        assert_eq!(action, Action::Run("goto 0 64 0".to_owned()));

        // While Baritone walks, nothing new happens.
        let walking = Snapshot {
            baritone_active: true,
            status: Some("walking".to_owned()),
            ..Default::default()
        };
        assert_eq!(engine.tick(&walking, &Watchdog::default()), Action::Nothing);

        // Baritone reached the goal and went idle: the step is let go and
        // the second one is submitted.
        let idle = Snapshot::default();
        assert_eq!(engine.tick(&idle, &Watchdog::default()), Action::Nothing);
        assert_eq!(engine.tick(&idle, &Watchdog::default()), Action::Nothing);
        match engine.tick(&idle, &Watchdog::default()) {
            Action::Done {
                cancel: false,
                message,
            } => {
                assert!(message.contains("goto 0 64 0"), "{message}");
                assert!(message.contains("1 queued"), "{message}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            engine.tick(&idle, &Watchdog::default()),
            Action::Run("mine 64 diamond_ore".to_owned())
        );
    }

    /// A count limit ends the step once the inventory holds the wanted
    /// number MORE than it did at the start — and the glue cancels
    /// Baritone, whose own goal was 64 ore blocks, not 64 diamonds.
    #[test]
    fn a_count_limit_stops_the_step_and_cancels() {
        let mut engine = Engine::new();
        engine.queue_entries(entries("mine 64 diamond_ore until 64 diamond"));
        let empty = Snapshot::default();
        assert_eq!(
            engine.tick(&empty, &Watchdog::default()),
            Action::Run("mine 64 diamond_ore".to_owned())
        );

        // Baritone mines; diamonds come in slowly, then past the goal.
        let mining = |counted: u32| Snapshot {
            baritone_active: true,
            counted,
            ..Default::default()
        };
        engine.tick(&mining(10), &Watchdog::default());
        engine.tick(&mining(63), &Watchdog::default());
        match engine.tick(&mining(64), &Watchdog::default()) {
            Action::Done {
                cancel: true,
                message,
            } => {
                assert!(message.contains("collected 64 of 64"), "{message}");
            }
            other => panic!("{other:?}"),
        }
    }

    /// The count measures the gain, not the total: diamonds already in the
    /// inventory at the start do not count toward the 64.
    #[test]
    fn a_count_limit_measures_the_gain() {
        let mut engine = Engine::new();
        engine.queue_entries(entries("mine diamond_ore until 4 diamond"));
        // Baritone is idle when the step is submitted; the 3 diamonds held
        // are the baseline the limit measures from.
        let start = Snapshot {
            counted: 3,
            ..Default::default()
        };
        assert_eq!(
            engine.tick(&start, &Watchdog::default()),
            Action::Run("mine diamond_ore".to_owned())
        );
        // 3 held + 3 gained = 6 total, but the gain is 3 — not there yet.
        let mid = Snapshot {
            counted: 6,
            baritone_active: true,
            ..Default::default()
        };
        assert_eq!(engine.tick(&mid, &Watchdog::default()), Action::Nothing);
        let done = Snapshot {
            counted: 7,
            baritone_active: true,
            ..Default::default()
        };
        match engine.tick(&done, &Watchdog::default()) {
            Action::Done { cancel: true, .. } => {}
            other => panic!("{other:?}"),
        }
    }

    /// The full and timed limits end their steps.
    #[test]
    fn the_full_and_timed_limits_end_their_steps() {
        let mut engine = Engine::new();
        engine.queue_entries(entries("farm until full"));
        assert_eq!(
            engine.tick(&Snapshot::default(), &Watchdog::default()),
            Action::Run("farm".to_owned())
        );
        let full = Snapshot {
            baritone_active: true,
            inventory_full: true,
            ..Default::default()
        };
        match engine.tick(&full, &Watchdog::default()) {
            Action::Done {
                cancel: true,
                message,
            } => assert!(message.contains("filled"), "{message}"),
            other => panic!("{other:?}"),
        }

        let mut engine = Engine::new();
        engine.queue_entries(entries("follow steve for 2s"));
        assert_eq!(
            engine.tick(&Snapshot::default(), &Watchdog::default()),
            Action::Run("follow steve".to_owned())
        );
        let busy = Snapshot {
            baritone_active: true,
            ..Default::default()
        };
        // Two seconds is 40 ticks: nothing for the first 39, done on the 40th.
        for _ in 0..39 {
            assert_eq!(engine.tick(&busy, &Watchdog::default()), Action::Nothing);
        }
        match engine.tick(&busy, &Watchdog::default()) {
            Action::Done {
                cancel: true,
                message,
            } => assert!(message.contains("time up"), "{message}"),
            other => panic!("{other:?}"),
        }
    }

    /// A step whose command never starts does not wedge the queue: three
    /// idle ticks let it go.
    #[test]
    fn a_step_that_never_starts_does_not_wedge_the_queue() {
        let mut engine = Engine::new();
        engine.queue_entries(entries("mine not_a_block then goto home"));
        let idle = Snapshot::default();
        assert_eq!(
            engine.tick(&idle, &Watchdog::default()),
            Action::Run("mine not_a_block".to_owned())
        );
        assert_eq!(engine.tick(&idle, &Watchdog::default()), Action::Nothing);
        assert_eq!(engine.tick(&idle, &Watchdog::default()), Action::Nothing);
        match engine.tick(&idle, &Watchdog::default()) {
            Action::Done {
                cancel: false,
                message,
            } => {
                assert!(message.contains("baritone went idle"), "{message}")
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            engine.tick(&idle, &Watchdog::default()),
            Action::Run("goto home".to_owned())
        );
    }

    /// A pause holds the queue but not the watchdog; a resume lets it run.
    #[test]
    fn a_pause_holds_the_queue_but_not_the_watchdog() {
        let mut engine = Engine::new();
        engine.queue_entries(entries("goto home"));
        engine.set_paused(true);
        let snap = Snapshot {
            hostile_near: true,
            ..Default::default()
        };
        let watchdog = Watchdog {
            mobs_blocks: 64,
            ..Default::default()
        };
        match engine.tick(&snap, &watchdog) {
            Action::Leave(reason) => assert!(reason.contains("hostile mob"), "{reason}"),
            other => panic!("{other:?}"),
        }

        engine.reset();
        engine.queue_entries(entries("goto home"));
        engine.set_paused(true);
        assert_eq!(
            engine.tick(&Snapshot::default(), &Watchdog::default()),
            Action::Nothing
        );
        engine.set_paused(false);
        assert_eq!(
            engine.tick(&Snapshot::default(), &Watchdog::default()),
            Action::Run("goto home".to_owned())
        );
    }

    /// The watchdog fires on each armed trigger — mobs, night, health,
    /// hunger — and never while the player is dead.
    #[test]
    fn the_watchdog_fires_on_each_trigger() {
        let mut engine = Engine::new();
        let watchdog = Watchdog {
            mobs_blocks: 256,
            night: true,
            health_below: 6.0,
            hunger_below: 4,
        };
        let mut case = |snap: Snapshot| {
            engine.reset();
            engine.tick(&snap, &Watchdog::default());
            match engine.tick(&snap, &watchdog) {
                Action::Leave(reason) => reason,
                other => panic!("{other:?}"),
            }
        };
        assert!(
            case(Snapshot {
                hostile_near: true,
                ..Default::default()
            })
            .contains("hostile")
        );
        assert!(
            case(Snapshot {
                night: true,
                ..Default::default()
            })
            .contains("night")
        );
        assert!(
            case(Snapshot {
                health: 5.0,
                ..Default::default()
            })
            .contains("health")
        );
        assert!(
            case(Snapshot {
                food: 3,
                ..Default::default()
            })
            .contains("hunger")
        );
        // Nothing armed fires when nothing is near.
        engine.reset();
        assert_eq!(
            engine.tick(&Snapshot::default(), &watchdog),
            Action::Nothing
        );
        // The dead player is not a surprise to react to.
        engine.reset();
        let dead = Snapshot {
            alive: false,
            health: 0.0,
            food: 0,
            hostile_near: true,
            night: true,
            ..Default::default()
        };
        assert_eq!(engine.tick(&dead, &watchdog), Action::Nothing);
        // An empty queue with the watchdog armed still watches.
        assert!(watchdog.armed());
    }

    /// The watchdog fires once: the leave is asked for, and the ticks until
    /// the world actually closes stay quiet.
    #[test]
    fn the_leave_fires_once_until_the_world_closes() {
        let mut engine = Engine::new();
        let watchdog = Watchdog {
            night: true,
            ..Default::default()
        };
        let snap = Snapshot {
            night: true,
            ..Default::default()
        };
        engine.tick(&snap, &Watchdog::default());
        match engine.tick(&snap, &watchdog) {
            Action::Leave(_) => {}
            other => panic!("{other:?}"),
        }
        assert_eq!(engine.tick(&snap, &watchdog), Action::Nothing);
        // And the world closing forgets everything — including the latch,
        // so the watchdog in a fresh world is armed again.
        let gone = Snapshot {
            in_world: false,
            ..Default::default()
        };
        assert_eq!(engine.tick(&gone, &Watchdog::default()), Action::Nothing);
        assert!(matches!(engine.tick(&snap, &watchdog), Action::Leave(_)));
    }

    /// skip and clear do what they say; a finished step is followed by the
    /// next submission once Baritone is free.
    #[test]
    fn skip_clear_and_waiting_work() {
        let mut engine = Engine::new();
        engine.queue_entries(entries("goto home then mine dirt"));
        assert_eq!(
            engine.tick(&Snapshot::default(), &Watchdog::default()),
            Action::Run("goto home".to_owned())
        );

        // Baritone walks, reaches the goal, goes idle: three idle ticks let
        // the step go, and the next one is submitted.
        let busy = Snapshot {
            baritone_active: true,
            ..Default::default()
        };
        engine.tick(&busy, &Watchdog::default());
        engine.tick(&busy, &Watchdog::default());
        let idle = Snapshot::default();
        assert_eq!(engine.tick(&idle, &Watchdog::default()), Action::Nothing);
        assert_eq!(engine.tick(&idle, &Watchdog::default()), Action::Nothing);
        match engine.tick(&idle, &Watchdog::default()) {
            Action::Done { cancel: false, .. } => {}
            other => panic!("{other:?}"),
        }
        assert_eq!(
            engine.tick(&idle, &Watchdog::default()),
            Action::Run("mine dirt".to_owned())
        );

        // skip drops the running step (the glue cancels Baritone) and
        // nothing follows it.
        assert!(
            engine.skip().unwrap().contains("mine dirt"),
            "skip names the step"
        );
        assert_eq!(engine.tick(&idle, &Watchdog::default()), Action::Nothing);

        // clear on an empty queue reports that it had nothing to drop.
        assert!(!engine.clear());
        assert_eq!(engine.status_lines(), vec!["the queue is empty".to_owned()]);
    }

    /// A queue waiting for a free Baritone submits the moment it is free.
    #[test]
    fn the_queue_waits_for_a_free_baritone() {
        let mut engine = Engine::new();
        engine.queue_entries(entries("goto home"));
        let busy = Snapshot {
            baritone_active: true,
            ..Default::default()
        };
        assert_eq!(engine.tick(&busy, &Watchdog::default()), Action::Nothing);
        assert_eq!(
            engine.tick(&Snapshot::default(), &Watchdog::default()),
            Action::Run("goto home".to_owned())
        );
    }

    /// A Baritone command the player types while a step runs takes over:
    /// the step is dropped, the queue is paused until `!q resume`, and a
    /// temporary process taking control does not count as that.
    #[test]
    fn the_player_taking_over_pauses_the_queue() {
        let mut engine = Engine::new();
        engine.queue_entries(entries("goto 0 64 0 then mine dirt"));
        assert_eq!(
            engine.tick(&Snapshot::default(), &Watchdog::default()),
            Action::Run("goto 0 64 0".to_owned())
        );

        // The step's own process takes control and is remembered.
        let ours = Snapshot {
            baritone_active: true,
            proc: Some("CustomGoalProcess".to_owned()),
            status: Some("walking".to_owned()),
            ..Default::default()
        };
        assert_eq!(engine.tick(&ours, &Watchdog::default()), Action::Nothing);

        // Baritone's own temporary helper taking control is not a takeover:
        // the glue filters those out of `proc`, so the step keeps its own.
        let temporary = Snapshot {
            baritone_active: true,
            proc: None,
            status: Some("getting to block".to_owned()),
            ..Default::default()
        };
        assert_eq!(
            engine.tick(&temporary, &Watchdog::default()),
            Action::Nothing
        );
        assert!(!engine.is_paused());

        // A different process of the player's own is: the step is dropped,
        // the queue is paused, and the player's goal is left alone.
        let theirs = Snapshot {
            baritone_active: true,
            proc: Some("MineProcess".to_owned()),
            ..Default::default()
        };
        match engine.tick(&theirs, &Watchdog::default()) {
            Action::Done {
                cancel: false,
                message,
            } => {
                assert!(message.contains("took over"), "{message}");
                assert!(message.contains("resume"), "{message}");
            }
            other => panic!("{other:?}"),
        }
        assert!(engine.is_paused());
        assert_eq!(
            engine.tick(&Snapshot::default(), &Watchdog::default()),
            Action::Nothing
        );
        engine.set_paused(false);
        assert_eq!(
            engine.tick(&Snapshot::default(), &Watchdog::default()),
            Action::Run("mine dirt".to_owned())
        );
    }

    /// A closed world forgets the queue; a new one starts fresh, and the
    /// thread-local engine answers through with_engine.
    #[test]
    fn a_closed_world_forgets_the_queue() {
        with_engine(|engine| {
            engine.reset();
            engine.queue_entries(entries("goto home"));
            assert_eq!(engine.status_lines().len(), 1);
        });
        let gone = Snapshot {
            in_world: false,
            ..Default::default()
        };
        with_engine(|engine| {
            assert_eq!(engine.tick(&gone, &Watchdog::default()), Action::Nothing);
            assert_eq!(engine.status_lines(), vec!["the queue is empty".to_owned()]);
        });
    }

    /// The help names the grammar.
    #[test]
    fn the_help_names_the_grammar() {
        let lines = help_lines();
        assert!(lines.iter().any(|l| l.contains("until <n> <item>")));
        assert!(lines.iter().any(|l| l.contains("!q leave")));
    }
}
