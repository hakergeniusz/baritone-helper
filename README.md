# Baritone Helper

A Horizon native mod that wraps [Baritone](https://playhorizon.dev/mods/baritone)
(the port of leijurv's Baritone by luhnox) with the three things its commands
lack: **stacked actions**, **stop-at-goal limits**, and an **auto-leave watchdog**.
Type commands in chat with the `!q` prefix — Baritone's own is `#`, so a line can
never mean both.

**Requires the Baritone mod** to be installed alongside: the helper drives
Baritone through Baritone's own command dispatcher and reports with its chat.

## Stacked actions

Steps are Baritone commands joined by ` then `, run one at a time — the next
starts once the previous is done:

```
!q goto 100 64 -200 then mine 64 diamond_ore then goto home
```

While the queue runs, Baritone's status is reported between steps. Commands
that never start (a bad block name, an unreachable goal) are let go after a
few idle ticks instead of wedging the queue.

## Stop-at-goal limits

End a step with a limit and the helper cancels Baritone the moment the goal
is met — `mine 64 diamond_ore` would otherwise mine 64 *ore blocks*, not
64 *diamonds* (fortune and deepslate variants make those different things):

- `until <n> <item>` — stop once the inventory holds **n more** of the item
  than it did when the step started (`… until 64 diamond`). Bare names
  (`diamond`) match their namespaced ones.
- `until full` — stop once no inventory slot is empty.
- `for <seconds>[s|m]` — stop after that long (`follow steve for 90s`, `explore 5m`).

## Auto-leave watchdog

`!q leave …` arms triggers that **close the world** the moment one fires —
Baritone is cancelled first, the reason is said in red, and the watchdog
latches until the world is actually gone (it stays armed while the queue is
paused, and holds its tongue on the death screen):

```
!q leave mobs 16     # a hostile mob within 16 chunks
!q leave night on    # the vanilla night window (12542–23459)
!q leave health 6    # health below 6 half-hearts
!q leave hunger 8    # food below 8
!q leave off         # everything off
!q leave             # what is armed
```

The triggers persist in the mod's settings storage.

## Queue control

```
!q status   # the queue, Baritone's own process, the armed triggers
!q skip     # drop the running step (Baritone is cancelled), keep the rest
!q clear    # drop the running step and the whole queue
!q pause    # hold the queue (the watchdog stays armed)
!q resume   # let it run again
!q help
```

If you type a Baritone command of your own (`#…`) while a step runs, your
command wins: the step is dropped, the queue is **paused** (so "then mine 64
diamonds" does not sneak up behind your escape route), and the helper says so —
`!q resume` continues. A `#` typed while the queue runs is noticed on the
following tick by which process is in control, so it works whichever hook
Baritone's chat runs through.

## How it fits together

The mod is a Horizon native mod (mixin bundle). Its brain — the queue engine,
the limits, the watchdog policy and the `!q` grammar — is plain standard
library Rust, injected into the game as the `baritone_helper` module and
doubled as the crate's library so it is tested on its own (17 tests). Three
glue helpers feed it a `Snapshot` of the world each tick (`native_tick`),
answer typed `!q` lines (`handle_chat_input`) and put its row in
Options > Mods (`native_mods`); they carry out the engine's answer through
Baritone's own surfaces (`baritone_command::run`, `cancel_everything`,
`describe`) and close the world with the game's own exit action when the
watchdog fires.

Verification recipe (pinned Horizon revision, Baritone co-staged by the tool):

```sh
python3 -I tools/test.py                  # fmt, clippy, the engine's tests
python3 -I tools/horizon.py check         # staged cargo check (Baritone applied alongside)
python3 -I tools/horizon.py test          # staged engine tests
python3 -I tools/horizon.py build         # staged release-ish build
python3 -I tools/package.py               # target/baritone_helper-0.1.0.hnmod
```

Set `BARITONE_HNMOD=/path/to/baritone-1.1.52.hnmod` to use a local copy —
otherwise the pinned release archive (SHA-256 checked) is fetched once and
cached under `target/vendor/`. The registry id is `baritone_helper`.

## License

MIT — see [LICENSE](LICENSE) and [CREDITS.md](CREDITS.md).
