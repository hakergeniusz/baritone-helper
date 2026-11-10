// The mod's row in Options > Mods: no settings rows of its own — the
// watchdog's triggers are set with `!q leave …` in chat and persist through
// the mod's settings storage.
// Copyright (c) 2026 hakergeniusz. MIT.
fn baritone_helper_listed(mods: &mut Vec<crate::game::native_hooks::NativeMod>) {
    use crate::game::baritone_helper as helper;
    mods.push(crate::game::native_hooks::NativeMod {
        id: helper::ID.into(),
        title: format!("Baritone Helper {}", helper::VERSION),
        description: helper::DESCRIPTION.into(),
        keys: vec![],
    });
}
