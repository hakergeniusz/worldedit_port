// A command typed here, before the game parses it (`Game::run_command`): the
// editor's whole game side, nested inside the one hook that reaches it. The
// decisions live in the brain crate (`worldedit::`), which holds the command
// table, the region math, the schematics folder and the legacy block table;
// everything that reads or writes the game — the registry, the world, the
// messages, the schematics menu — is nested below.
//
// The editor belongs to the game that owns the world: single player's server
// thread, the machine hosting a LAN world. It writes with `Game::level_set`
// (the `/setblock` path: neighbour updates, block entities, client sync, level
// events), so no command, packet or block log is involved and there is no
// command length or count limit. A client of a server never edits: it sends
// the command and the server's own copy does the work.
//
// One editor state per world (`world_root`), taken out of a thread-local map
// for the duration of a call and put back afterwards, so nothing of the mod is
// borrowed while game code runs and no field is added to `Game`.
// Copyright (c) 2026 hakergeniusz. MIT.
fn worldedit_command(
    game: &mut crate::game::Game<'_>,
    command: &str,
) -> core::ops::ControlFlow<bool> {
    use horizon_client::items::ItemStack;
    use horizon_core::nbt::{self, Limits, NamedTag, Tag};
    use horizon_world::registry::BlockStateId;
    use std::collections::{BTreeMap, HashMap, HashSet};
    use std::path::{Path, PathBuf};

    type Pos = worldedit::Pos;

    /// The editor's state for one world.
    #[derive(Default)]
    struct State {
        pos1: Option<Pos>,
        pos2: Option<Pos>,
        /// Clipboard blocks, relative to the paste origin.
        clipboard: Vec<(Pos, BlockStateId)>,
        /// What is in the clipboard, for `//paste`'s report.
        clipboard_name: String,
        /// Each edit's previous blocks, newest last.
        undo: Vec<Vec<(Pos, BlockStateId)>>,
        redo: Vec<Vec<(Pos, BlockStateId)>>,
        /// Bumped per edit, so a pattern spreads differently each time.
        salt: u64,
    }
    thread_local! {
        static STATES: std::cell::RefCell<BTreeMap<PathBuf, State>> =
            const { std::cell::RefCell::new(BTreeMap::new()) };
    }

    /// Runs `run` with this world's editor state. The state is taken out for
    /// the call and put back afterwards, so game code never runs under the
    /// borrow.
    fn with_state<T>(
        game: &mut crate::game::Game<'_>,
        run: impl FnOnce(&mut crate::game::Game<'_>, &mut State) -> T,
    ) -> T {
        let key = game.world_root.clone();
        let mut state = STATES
            .with(|states| states.borrow_mut().remove(&key).unwrap_or_default());
        let out = run(game, &mut state);
        STATES.with(|states| states.borrow_mut().insert(key, state));
        out
    }

    fn message(game: &mut crate::game::Game<'_>, text: impl Into<String>) {
        game.system_message(horizon_client::text::Text::literal(text.into()));
    }
    fn failure(game: &mut crate::game::Game<'_>, text: impl Into<String>) {
        game.command_failure(horizon_client::text::Text::literal(text.into()));
    }
    fn too_big(game: &mut crate::game::Game<'_>, volume: i64) {
        failure(
            game,
            format!("That region is {volume} blocks; the limit is {}", worldedit::MAX_VOLUME),
        );
    }

    /// The acting player's feet: `~` is measured from here.
    fn player_pos(game: &crate::game::Game<'_>) -> Pos {
        let Some(player) = game.player.as_ref() else {
            return [0, 0, 0];
        };
        [
            player.position.x.floor() as i32,
            player.position.y.floor() as i32,
            player.position.z.floor() as i32,
        ]
    }

    /// `x y z`, each a number or `~` / `~offset`.
    fn parse_triple(game: &crate::game::Game<'_>, args: &[&str]) -> Option<Pos> {
        if args.len() < 3 {
            return None;
        }
        let base = player_pos(game);
        let mut out = base;
        for (i, arg) in args.iter().take(3).enumerate() {
            match arg.strip_prefix('~') {
                Some(rest) if rest.is_empty() => out[i] = base[i],
                Some(rest) => {
                    let offset = rest.parse::<f64>().ok()?;
                    out[i] = base[i].saturating_add(offset.floor() as i32);
                }
                None => out[i] = arg.parse::<f64>().ok()?.floor() as i32,
            }
        }
        Some(out)
    }

    /// A block name (`stone`, `minecraft:oak_stairs[facing=east]`) as a state.
    fn block_state(game: &crate::game::Game<'_>, spec: &str) -> Option<BlockStateId> {
        let registry = &game.pack.assets.registry;
        let (name, properties) = match spec.split_once('[') {
            Some((name, rest)) => (name.trim(), rest.strip_suffix(']')?),
            None => (spec.trim(), ""),
        };
        let name = if name.contains(':') {
            name.to_owned()
        } else {
            format!("minecraft:{name}")
        };
        let default = registry.default_state(&name)?;
        if properties.trim().is_empty() {
            return Some(default);
        }
        let mut state = registry.state(default)?.clone();
        for pair in properties.split(',') {
            let (key, value) = pair.split_once('=')?;
            state
                .properties
                .insert(key.trim().to_owned(), value.trim().to_owned());
        }
        registry.resolve(&state).ok()
    }
    fn air(game: &crate::game::Game<'_>) -> BlockStateId {
        block_state(game, "minecraft:air").expect("minecraft:air is in the registry")
    }
    fn block_name(game: &crate::game::Game<'_>, state: BlockStateId) -> String {
        // An id the world handed out always has facts; the fallback never
        // shows.
        game.res.facts().get(state).name.to_string()
    }

    /// A block pattern: one block, or a weighted list (`stone,dirt`,
    /// `70%stone,30%gravel`) picked at random for each block of a region.
    struct Pattern {
        /// The text as typed, for messages.
        text: String,
        /// (weight, block), weight at least 1.
        entries: Vec<(u32, BlockStateId)>,
        total: u32,
    }
    impl Pattern {
        fn of(block: BlockStateId, text: String) -> Self {
            Self {
                text,
                entries: vec![(1, block)],
                total: 1,
            }
        }
        /// The block for `pos`: the only one, or a weighted pick.
        fn pick(&self, salt: u64, pos: Pos) -> BlockStateId {
            if self.entries.len() == 1 {
                return self.entries[0].1;
            }
            let mut roll = (worldedit::mix(salt, pos) % u64::from(self.total)) as u32;
            for &(weight, block) in &self.entries {
                if roll < weight {
                    return block;
                }
                roll -= weight;
            }
            self.entries[self.entries.len() - 1].1
        }
    }

    /// A pattern, or None when a name in it is not a block.
    fn pattern(game: &crate::game::Game<'_>, spec: &str) -> Option<Pattern> {
        let mut entries = Vec::new();
        let mut total = 0u32;
        for part in worldedit::split_entries(spec) {
            let (weight, name) = worldedit::weighted(&part)?;
            if name.is_empty() {
                return None;
            }
            let block = block_state(game, name)?;
            total += weight;
            entries.push((weight, block));
        }
        if entries.is_empty() {
            return None;
        }
        Some(Pattern {
            text: spec.trim().to_owned(),
            entries,
            total,
        })
    }

    /// What a report calls a pattern: the block, or the list as typed.
    fn pattern_label(game: &crate::game::Game<'_>, pattern: &Pattern) -> String {
        if pattern.entries.len() == 1 {
            block_name(game, pattern.entries[0].1)
        } else {
            pattern.text.clone()
        }
    }

    /// Which blocks a mask accepts: any entry of `stone,dirt`, an exact state
    /// when properties were given.
    struct Mask {
        parts: Vec<MaskPart>,
    }
    struct MaskPart {
        name: String,
        exact: Option<BlockStateId>,
    }
    impl Mask {
        fn hits(&self, game: &crate::game::Game<'_>, state: BlockStateId) -> bool {
            self.parts.iter().any(|part| match part.exact {
                Some(exact) => state == exact,
                None => game.res.facts().get(state).name.as_ref() == part.name.as_str(),
            })
        }
    }
    fn mask(game: &crate::game::Game<'_>, spec: &str) -> Option<Mask> {
        let mut parts = Vec::new();
        for entry in worldedit::split_entries(spec) {
            let (_, name) = worldedit::weighted(&entry)?;
            if name.is_empty() {
                return None;
            }
            let state = block_state(game, name)?;
            parts.push(MaskPart {
                name: block_name(game, state),
                exact: name.contains('[').then_some(state),
            });
        }
        if parts.is_empty() {
            return None;
        }
        Some(Mask { parts })
    }

    /// The selection's low corner, high corner and volume, clipped to the
    /// build height.
    fn region(
        game: &crate::game::Game<'_>,
        state: &State,
    ) -> Option<(Pos, Pos, i64)> {
        worldedit::region_of(
            state.pos1,
            state.pos2,
            game.world.shape.min_y,
            game.world.shape.max_y(),
        )
    }

    /// The selection, or a message saying there is none and how to make one.
    fn selection(
        game: &mut crate::game::Game<'_>,
        state: &State,
    ) -> Option<(Pos, Pos, i64)> {
        match region(game, state) {
            Some(region) => Some(region),
            None => {
                failure(
                    game,
                    "Select a region first: //pos1 and //pos2, or sneak + click with the wand",
                );
                None
            }
        }
    }

    /// Writes `ops`, remembering what was there; one `//undo` step for the
    /// lot.
    fn place_ops(
        game: &mut crate::game::Game<'_>,
        state: &mut State,
        ops: &[(Pos, BlockStateId)],
    ) -> usize {
        let mut history: Vec<(Pos, BlockStateId)> = Vec::new();
        for &(pos, block) in ops {
            let old = game.world.block(pos[0], pos[1], pos[2]);
            if old == block {
                continue;
            }
            if game.level_set(pos, block).is_ok() {
                history.push((pos, old));
            }
        }
        let changed = history.len();
        if changed > 0 {
            state.undo.push(history);
            if state.undo.len() > worldedit::MAX_HISTORY {
                state.undo.remove(0);
            }
            state.redo.clear();
        }
        changed
    }
    fn set_positions(
        game: &mut crate::game::Game<'_>,
        state: &mut State,
        positions: &[Pos],
        pattern: &Pattern,
    ) -> usize {
        state.salt = worldedit::next_salt(state.salt);
        let salt = state.salt;
        let ops: Vec<(Pos, BlockStateId)> = positions
            .iter()
            .map(|&pos| (pos, pattern.pick(salt, pos)))
            .collect();
        place_ops(game, state, &ops)
    }
    fn report_set(game: &mut crate::game::Game<'_>, name: &str, count: usize) {
        message(
            game,
            format!(
                "{count} block{} set to {name}",
                if count == 1 { "" } else { "s" }
            ),
        );
    }

    /// Every position of `lo..=hi` whose block `keep` accepts, skipping
    /// chunks that are not loaded (an edit never loads one).
    fn collect(
        game: &crate::game::Game<'_>,
        lo: Pos,
        hi: Pos,
        keep: impl Fn(&crate::game::Game<'_>, BlockStateId) -> bool,
    ) -> Vec<Pos> {
        let mut out = Vec::new();
        for x in lo[0]..=hi[0] {
            for z in lo[2]..=hi[2] {
                if !game.world.loaded(x, z) {
                    continue;
                }
                for y in lo[1]..=hi[1] {
                    let old = game.world.block(x, y, z);
                    if keep(game, old) {
                        out.push([x, y, z]);
                    }
                }
            }
        }
        out
    }

    fn describe(game: &mut crate::game::Game<'_>, lo: Pos, hi: Pos, volume: i64) {
        message(
            game,
            format!(
                "Region {}, {}, {} to {}, {}, {} ({}x{}x{}, {volume} blocks)",
                lo[0],
                lo[1],
                lo[2],
                hi[0],
                hi[1],
                hi[2],
                hi[0] - lo[0] + 1,
                hi[1] - lo[1] + 1,
                hi[2] - lo[2] + 1,
            ),
        );
    }

    /// The wooden axe `//wand` gives: named, and that name is what marks it.
    fn wand_stack() -> ItemStack {
        let mut stack = ItemStack::new(worldedit::WAND_ITEM, 1);
        stack.data_mut().custom_name = Some(Tag::String(worldedit::WAND_NAME.into()));
        stack
    }
    /// Whether `stack` is the wand `//wand` handed out.
    fn is_the_wand(stack: &ItemStack) -> bool {
        let name = stack
            .data()
            .and_then(|data| data.custom_name.as_ref())
            .and_then(|tag| match tag {
                Tag::String(value) => Some(value.to_string_lossy()),
                _ => None,
            });
        worldedit::is_wand(&stack.item, name.as_deref())
    }
    /// `//wand`: the wooden axe, into the inventory.
    fn give_wand(game: &mut crate::game::Game<'_>) -> bool {
        // `/give`'s handing over: the inventory takes what fits, and the rest
        // is thrown from the eyes for the player to pick up.
        game.give_acting_stack(wand_stack(), None);
        message(
            game,
            "WorldEdit wand given: left click a block sets position 1, right click sets position 2",
        );
        true
    }
    fn help(game: &mut crate::game::Game<'_>) {
        for line in worldedit::HELP {
            message(game, *line);
        }
    }

    /// A vanilla command name: this mod may not take it.
    fn vanilla_owns(name: &str) -> bool {
        let dispatcher = horizon_client::command::vanilla::game_dispatcher();
        dispatcher.nodes[0]
            .children
            .iter()
            .any(|&child| dispatcher.nodes[child].name() == name)
    }
    /// The command `command` holds, or None when this mod does not own it.
    fn parse(command: &str) -> Option<(String, Vec<&str>)> {
        let (name, args) = worldedit::parse_base(command)?;
        if vanilla_owns(&name) {
            return None;
        }
        Some((name, args))
    }

    /// The editor runs the command. False when it refused: the caller's line
    /// went nowhere else, so a refusal is a message, not a silence.
    fn run(game: &mut crate::game::Game<'_>, name: &str, args: &[&str]) -> bool {
        if game.remote.is_some() {
            return false;
        }
        if !matches!(name, "we" | "wand")
            && (!game.may_build() || game.ui.registries.permission < 2)
        {
            failure(game, "WorldEdit needs permission level 2 and build access");
            return false;
        }
        with_state(game, |game, state| match name {
            "we" => {
                help(game);
                true
            }
            "wand" => give_wand(game),
            "pos1" => set_position(game, state, true, args),
            "pos2" => set_position(game, state, false, args),
            "sel" => select(game, state, args),
            "expand" => resize(game, state, args, true),
            "contract" => resize(game, state, args, false),
            "shift" => shift(game, state, args),
            "set" => set(game, state, args),
            "replace" => replace(game, state, args),
            "walls" => shell(game, state, args, Shell::Walls),
            "outline" => shell(game, state, args, Shell::Outline),
            "hollow" => shell(game, state, args, Shell::Hollow),
            "faces" => shell(game, state, args, Shell::Faces),
            "overlay" => overlay(game, state, args),
            "center" => center(game, state, args),
            "count" => count(game, state, args),
            "distr" => distr(game, state),
            "sphere" => sphere(game, state, args, false),
            "hsphere" => sphere(game, state, args, true),
            "cyl" => cylinder(game, state, args, false),
            "hcyl" => cylinder(game, state, args, true),
            "pyramid" => pyramid(game, state, args, false),
            "hpyramid" => pyramid(game, state, args, true),
            "line" => line(game, state, args),
            "copy" => copy(game, state, false),
            "cut" => copy(game, state, true),
            "paste" => paste(game, state),
            "move" => move_selection(game, state, args),
            "undo" => undo(game, state),
            "redo" => redo(game, state),
            "schem" => schem(game, state, args),
            "load" => load_schematic(game, state, args),
            // `//clearhistory`, and anything the table grows later.
            _ => {
                state.undo.clear();
                state.redo.clear();
                message(game, "History cleared");
                true
            }
        })
    }

    fn set_position(
        game: &mut crate::game::Game<'_>,
        state: &mut State,
        first: bool,
        args: &[&str],
    ) -> bool {
        let pos = if args.is_empty() {
            player_pos(game)
        } else if let Some(pos) = parse_triple(game, args) {
            pos
        } else {
            failure(game, "Usage: //pos1 [x y z] (~ for where you stand)");
            return false;
        };
        if first {
            state.pos1 = Some(pos);
        } else {
            state.pos2 = Some(pos);
        }
        message(
            game,
            format!(
                "Position {} set to {}, {}, {}",
                if first { 1 } else { 2 },
                pos[0],
                pos[1],
                pos[2]
            ),
        );
        if let Some((lo, hi, volume)) = region(game, state) {
            describe(game, lo, hi, volume);
        }
        true
    }
    fn select(
        game: &mut crate::game::Game<'_>,
        state: &mut State,
        args: &[&str],
    ) -> bool {
        if args.first() == Some(&"clear") {
            state.pos1 = None;
            state.pos2 = None;
            message(game, "Selection cleared");
            return true;
        }
        let (Some(a), Some(b)) = (state.pos1, state.pos2) else {
            message(game, "Selection: no corners yet (//pos1 and //pos2)");
            return true;
        };
        match region(game, state) {
            Some((lo, hi, volume)) => describe(game, lo, hi, volume),
            None => message(
                game,
                format!(
                    "Selection: {}, {}, {} to {}, {}, {} (outside the build height)",
                    a[0], a[1], a[2], b[0], b[1], b[2]
                ),
            ),
        }
        true
    }
    fn resize(
        game: &mut crate::game::Game<'_>,
        state: &mut State,
        args: &[&str],
        grow: bool,
    ) -> bool {
        let Some((mut lo, mut hi, _)) = selection(game, state) else {
            return false;
        };
        let amount: i32 = args
            .first()
            .and_then(|a| a.parse::<i32>().ok())
            .unwrap_or(1)
            .abs()
            .max(1);
        let directions: Vec<(usize, bool)> = match args.get(1) {
            Some(word) => match worldedit::direction(word) {
                Some(d) => vec![d],
                None => {
                    failure(
                        game,
                        "Usage: //expand|//contract <amount> [up|down|north|south|east|west]",
                    );
                    return false;
                }
            },
            None => {
                vec![
                    (0, true),
                    (0, false),
                    (1, true),
                    (1, false),
                    (2, true),
                    (2, false),
                ]
            }
        };
        let delta = if grow { amount } else { -amount };
        for (axis, positive) in directions {
            if positive {
                hi[axis] = hi[axis].saturating_add(delta);
                if hi[axis] < lo[axis] {
                    hi[axis] = lo[axis];
                }
            } else {
                lo[axis] = lo[axis].saturating_sub(delta);
                if lo[axis] > hi[axis] {
                    lo[axis] = hi[axis];
                }
            }
        }
        lo[1] = lo[1].max(game.world.shape.min_y);
        hi[1] = hi[1].min(game.world.shape.max_y());
        state.pos1 = Some(lo);
        state.pos2 = Some(hi);
        if let Some((lo, hi, volume)) = region(game, state) {
            describe(game, lo, hi, volume);
        }
        true
    }
    fn shift(
        game: &mut crate::game::Game<'_>,
        state: &mut State,
        args: &[&str],
    ) -> bool {
        let Some((lo, hi, _)) = selection(game, state) else {
            return false;
        };
        let amount: i32 = args
            .first()
            .and_then(|a| a.parse::<i32>().ok())
            .unwrap_or(1)
            .abs()
            .max(1);
        let Some(word) = args.get(1) else {
            failure(
                game,
                "Usage: //shift <amount> <up|down|north|south|east|west>",
            );
            return false;
        };
        let Some((axis, positive)) = worldedit::direction(word) else {
            failure(
                game,
                "Usage: //shift <amount> <up|down|north|south|east|west>",
            );
            return false;
        };
        let delta = if positive { amount } else { -amount };
        let moved = |p: Pos| {
            let mut p = p;
            p[axis] = p[axis].saturating_add(delta);
            p
        };
        state.pos1 = Some(moved(lo));
        state.pos2 = Some(moved(hi));
        if let Some((lo, hi, volume)) = region(game, state) {
            describe(game, lo, hi, volume);
        }
        true
    }
    fn set(
        game: &mut crate::game::Game<'_>,
        state: &mut State,
        args: &[&str],
    ) -> bool {
        let Some((lo, hi, volume)) = selection(game, state) else {
            return false;
        };
        let Some(spec) = args.first() else {
            failure(game, "Usage: //set <block>[,<block>]");
            return false;
        };
        if volume > worldedit::MAX_VOLUME {
            too_big(game, volume);
            return false;
        }
        let Some(pattern) = pattern(game, spec) else {
            failure(game, format!("Unknown block {spec}"));
            return false;
        };
        let positions = collect(game, lo, hi, |_, _| true);
        let label = pattern_label(game, &pattern);
        let changed = set_positions(game, state, &positions, &pattern);
        report_set(game, &label, changed);
        true
    }
    fn replace(
        game: &mut crate::game::Game<'_>,
        state: &mut State,
        args: &[&str],
    ) -> bool {
        let Some((lo, hi, volume)) = selection(game, state) else {
            return false;
        };
        let Some(from) = args.first() else {
            failure(game, "Usage: //replace <from>[,<from>] [to]");
            return false;
        };
        if volume > worldedit::MAX_VOLUME {
            too_big(game, volume);
            return false;
        }
        let Some(matches) = mask(game, from) else {
            failure(game, format!("Unknown block {from}"));
            return false;
        };
        let to = match args.get(1) {
            Some(spec) => match pattern(game, spec) {
                Some(pattern) => pattern,
                None => {
                    failure(game, format!("Unknown block {spec}"));
                    return false;
                }
            },
            None => Pattern::of(air(game), "minecraft:air".to_owned()),
        };
        let positions = collect(game, lo, hi, |game, old| matches.hits(game, old));
        let label = pattern_label(game, &to);
        let changed = set_positions(game, state, &positions, &to);
        report_set(game, &label, changed);
        true
    }
    enum Shell {
        Walls,
        Outline,
        Hollow,
        Faces,
    }
    fn shell(
        game: &mut crate::game::Game<'_>,
        state: &mut State,
        args: &[&str],
        shape: Shell,
    ) -> bool {
        let Some((lo, hi, volume)) = selection(game, state) else {
            return false;
        };
        let Some(spec) = args.first() else {
            failure(
                game,
                "Usage: //walls|//outline|//hollow|//faces <block>[,<block>]",
            );
            return false;
        };
        if volume > worldedit::MAX_VOLUME {
            too_big(game, volume);
            return false;
        }
        let Some(pattern) = pattern(game, spec) else {
            failure(game, format!("Unknown block {spec}"));
            return false;
        };
        state.salt = worldedit::next_salt(state.salt);
        let salt = state.salt;
        let air = air(game);
        let mut ops: Vec<(Pos, BlockStateId)> = Vec::new();
        for x in lo[0]..=hi[0] {
            for z in lo[2]..=hi[2] {
                if !game.world.loaded(x, z) {
                    continue;
                }
                for y in lo[1]..=hi[1] {
                    let pos = [x, y, z];
                    let on_x = x == lo[0] || x == hi[0];
                    let on_y = y == lo[1] || y == hi[1];
                    let on_z = z == lo[2] || z == hi[2];
                    let interior = !on_x && !on_y && !on_z;
                    let is_shell = match shape {
                        Shell::Walls => on_x || on_z,
                        Shell::Outline | Shell::Faces => on_x || on_y || on_z,
                        Shell::Hollow => !interior,
                    };
                    if is_shell {
                        ops.push((pos, pattern.pick(salt, pos)));
                    } else if matches!(shape, Shell::Hollow) {
                        ops.push((pos, air));
                    }
                }
            }
        }
        let label = pattern_label(game, &pattern);
        let changed = place_ops(game, state, &ops);
        report_set(game, &label, changed);
        true
    }
    fn overlay(
        game: &mut crate::game::Game<'_>,
        state: &mut State,
        args: &[&str],
    ) -> bool {
        let Some((lo, hi, volume)) = selection(game, state) else {
            return false;
        };
        let Some(spec) = args.first() else {
            failure(game, "Usage: //overlay <block>[,<block>]");
            return false;
        };
        if volume > worldedit::MAX_VOLUME {
            too_big(game, volume);
            return false;
        }
        let Some(pattern) = pattern(game, spec) else {
            failure(game, format!("Unknown block {spec}"));
            return false;
        };
        let mut positions = Vec::new();
        for x in lo[0]..=hi[0] {
            for z in lo[2]..=hi[2] {
                if !game.world.loaded(x, z) {
                    continue;
                }
                for y in (lo[1]..=hi[1]).rev() {
                    if !game.res.facts().is_air(game.world.block(x, y, z)) {
                        positions.push([x, y, z]);
                        break;
                    }
                }
            }
        }
        let label = pattern_label(game, &pattern);
        let changed = set_positions(game, state, &positions, &pattern);
        report_set(game, &label, changed);
        true
    }
    fn center(
        game: &mut crate::game::Game<'_>,
        state: &mut State,
        args: &[&str],
    ) -> bool {
        let Some((lo, hi, _)) = selection(game, state) else {
            return false;
        };
        let Some(spec) = args.first() else {
            failure(game, "Usage: //center <block>[,<block>]");
            return false;
        };
        let Some(pattern) = pattern(game, spec) else {
            failure(game, format!("Unknown block {spec}"));
            return false;
        };
        let middle = [
            (lo[0] + hi[0]) / 2,
            (lo[1] + hi[1]) / 2,
            (lo[2] + hi[2]) / 2,
        ];
        let label = pattern_label(game, &pattern);
        let changed = set_positions(game, state, &[middle], &pattern);
        report_set(game, &label, changed);
        true
    }
    fn count(
        game: &mut crate::game::Game<'_>,
        state: &mut State,
        args: &[&str],
    ) -> bool {
        let Some((lo, hi, _)) = selection(game, state) else {
            return false;
        };
        let Some(spec) = args.first() else {
            failure(game, "Usage: //count <block>[,<block>]");
            return false;
        };
        let Some(matches) = mask(game, spec) else {
            failure(game, format!("Unknown block {spec}"));
            return false;
        };
        let found = collect(game, lo, hi, |game, old| matches.hits(game, old)).len();
        message(
            game,
            format!(
                "{found} block{} of {spec} in the region",
                if found == 1 { "" } else { "s" }
            ),
        );
        true
    }
    fn distr(game: &mut crate::game::Game<'_>, state: &mut State) -> bool {
        let Some((lo, hi, volume)) = selection(game, state) else {
            return false;
        };
        let mut counts: HashMap<String, i64> = HashMap::new();
        let mut total = 0i64;
        for x in lo[0]..=hi[0] {
            for z in lo[2]..=hi[2] {
                if !game.world.loaded(x, z) {
                    continue;
                }
                for y in lo[1]..=hi[1] {
                    let name = block_name(game, game.world.block(x, y, z));
                    *counts.entry(name).or_default() += 1;
                    total += 1;
                }
            }
        }
        let mut rows: Vec<(String, i64)> = counts.into_iter().collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        message(
            game,
            format!("{total} blocks counted of {volume} in the region, by type:"),
        );
        for (name, count) in rows.into_iter().take(10) {
            let percent = if total > 0 {
                (count as f64) * 100.0 / (total as f64)
            } else {
                0.0
            };
            message(game, format!("  {name}: {count} ({percent:.1}%)"));
        }
        true
    }
    fn sphere(
        game: &mut crate::game::Game<'_>,
        state: &mut State,
        args: &[&str],
        mut hollow: bool,
    ) -> bool {
        let center = player_pos(game);
        let mut args = args;
        if args.first() == Some(&"-h") {
            hollow = true;
            args = &args[1..];
        }
        let Some(spec) = args.first() else {
            failure(game, "Usage: //sphere [-h] <block>[,<block>] <radius>");
            return false;
        };
        let radius: i32 = args
            .get(1)
            .and_then(|a| a.parse::<i32>().ok())
            .unwrap_or(4)
            .clamp(1, 128);
        let Some(pattern) = pattern(game, spec) else {
            failure(game, format!("Unknown block {spec}"));
            return false;
        };
        let inner = (radius - 1).max(0);
        let mut positions = Vec::new();
        for dx in -radius..=radius {
            for dy in -radius..=radius {
                for dz in -radius..=radius {
                    let (x, y, z) = (center[0] + dx, center[1] + dy, center[2] + dz);
                    if y < game.world.shape.min_y
                        || y > game.world.shape.max_y()
                        || !game.world.loaded(x, z)
                    {
                        continue;
                    }
                    let distance = dx * dx + dy * dy + dz * dz;
                    if distance > radius * radius || (hollow && distance <= inner * inner) {
                        continue;
                    }
                    positions.push([x, y, z]);
                }
            }
        }
        if positions.len() as i64 > worldedit::MAX_VOLUME {
            failure(game, "That sphere is too big");
            return false;
        }
        let label = pattern_label(game, &pattern);
        let changed = set_positions(game, state, &positions, &pattern);
        report_set(game, &label, changed);
        true
    }
    fn cylinder(
        game: &mut crate::game::Game<'_>,
        state: &mut State,
        args: &[&str],
        mut hollow: bool,
    ) -> bool {
        let center = player_pos(game);
        let mut args = args;
        if args.first() == Some(&"-h") {
            hollow = true;
            args = &args[1..];
        }
        let Some(spec) = args.first() else {
            failure(
                game,
                "Usage: //cyl [-h] <block>[,<block>] <radius> [height]",
            );
            return false;
        };
        let radius: i32 = args
            .get(1)
            .and_then(|a| a.parse::<i32>().ok())
            .unwrap_or(4)
            .clamp(1, 128);
        let height: i32 = args
            .get(2)
            .and_then(|a| a.parse::<i32>().ok())
            .unwrap_or(1)
            .clamp(1, 256);
        let Some(pattern) = pattern(game, spec) else {
            failure(game, format!("Unknown block {spec}"));
            return false;
        };
        let inner = (radius - 1).max(0);
        let mut positions = Vec::new();
        for y in center[1]..(center[1] + height) {
            if y < game.world.shape.min_y || y > game.world.shape.max_y() {
                continue;
            }
            for dx in -radius..=radius {
                for dz in -radius..=radius {
                    let distance = dx * dx + dz * dz;
                    if distance > radius * radius || (hollow && distance <= inner * inner) {
                        continue;
                    }
                    let (x, z) = (center[0] + dx, center[2] + dz);
                    if !game.world.loaded(x, z) {
                        continue;
                    }
                    positions.push([x, y, z]);
                }
            }
        }
        if positions.len() as i64 > worldedit::MAX_VOLUME {
            failure(game, "That cylinder is too big");
            return false;
        }
        let label = pattern_label(game, &pattern);
        let changed = set_positions(game, state, &positions, &pattern);
        report_set(game, &label, changed);
        true
    }
    fn pyramid(
        game: &mut crate::game::Game<'_>,
        state: &mut State,
        args: &[&str],
        mut hollow: bool,
    ) -> bool {
        let base = player_pos(game);
        let mut args = args;
        if args.first() == Some(&"-h") {
            hollow = true;
            args = &args[1..];
        }
        let Some(spec) = args.first() else {
            failure(game, "Usage: //pyramid [-h] <block>[,<block>] <size>");
            return false;
        };
        let size: i32 = args
            .get(1)
            .and_then(|a| a.parse::<i32>().ok())
            .unwrap_or(4)
            .clamp(1, 64);
        let Some(pattern) = pattern(game, spec) else {
            failure(game, format!("Unknown block {spec}"));
            return false;
        };
        let mut positions = Vec::new();
        for layer in 0..size {
            let y = base[1] + layer;
            if y > game.world.shape.max_y() {
                break;
            }
            let radius = size - 1 - layer;
            for dx in -radius..=radius {
                for dz in -radius..=radius {
                    if hollow && layer > 0 && dx.abs() < radius && dz.abs() < radius {
                        continue;
                    }
                    let (x, z) = (base[0] + dx, base[2] + dz);
                    if !game.world.loaded(x, z) {
                        continue;
                    }
                    positions.push([x, y, z]);
                }
            }
        }
        if positions.len() as i64 > worldedit::MAX_VOLUME {
            failure(game, "That pyramid is too big");
            return false;
        }
        let label = pattern_label(game, &pattern);
        let changed = set_positions(game, state, &positions, &pattern);
        report_set(game, &label, changed);
        true
    }
    fn line(
        game: &mut crate::game::Game<'_>,
        state: &mut State,
        args: &[&str],
    ) -> bool {
        let Some(spec) = args.first() else {
            failure(game, "Usage: //line <block>[,<block>] [thickness]");
            return false;
        };
        let thickness: i32 = args
            .get(1)
            .and_then(|a| a.parse::<i32>().ok())
            .unwrap_or(0)
            .clamp(0, 16);
        let Some(pattern) = pattern(game, spec) else {
            failure(game, format!("Unknown block {spec}"));
            return false;
        };
        let from = player_pos(game);
        let Some(to) = game.pick().map(|hit| hit.pos) else {
            failure(game, "Look at a block within reach to end the line");
            return false;
        };
        let steps = (to[0] - from[0])
            .abs()
            .max((to[1] - from[1]).abs())
            .max((to[2] - from[2]).abs())
            .max(1);
        let mut seen: HashSet<Pos> = HashSet::new();
        let mut positions = Vec::new();
        for step in 0..=steps {
            let t = f64::from(step) / f64::from(steps);
            let at = [
                from[0] + (((to[0] - from[0]) as f64) * t) as i32,
                from[1] + (((to[1] - from[1]) as f64) * t) as i32,
                from[2] + (((to[2] - from[2]) as f64) * t) as i32,
            ];
            for dx in -thickness..=thickness {
                for dy in -thickness..=thickness {
                    for dz in -thickness..=thickness {
                        let pos = [at[0] + dx, at[1] + dy, at[2] + dz];
                        if pos[1] < game.world.shape.min_y
                            || pos[1] > game.world.shape.max_y()
                        {
                            continue;
                        }
                        if seen.insert(pos) && game.world.loaded(pos[0], pos[2]) {
                            positions.push(pos);
                        }
                    }
                }
            }
        }
        if positions.len() as i64 > worldedit::MAX_VOLUME {
            failure(game, "That line is too big");
            return false;
        }
        let label = pattern_label(game, &pattern);
        let changed = set_positions(game, state, &positions, &pattern);
        report_set(game, &label, changed);
        true
    }
    fn copy(
        game: &mut crate::game::Game<'_>,
        state: &mut State,
        cut: bool,
    ) -> bool {
        let Some((lo, hi, volume)) = selection(game, state) else {
            return false;
        };
        if volume > worldedit::MAX_VOLUME {
            too_big(game, volume);
            return false;
        }
        let mut blocks = Vec::new();
        for x in lo[0]..=hi[0] {
            for z in lo[2]..=hi[2] {
                if !game.world.loaded(x, z) {
                    continue;
                }
                for y in lo[1]..=hi[1] {
                    blocks.push(([x - lo[0], y - lo[1], z - lo[2]], game.world.block(x, y, z)));
                }
            }
        }
        let copied = blocks.len();
        state.clipboard = blocks;
        state.clipboard_name = "the copied region".to_owned();
        if cut {
            let air = Pattern::of(air(game), "minecraft:air".to_owned());
            let positions = collect(game, lo, hi, |_, _| true);
            let removed = set_positions(game, state, &positions, &air);
            message(game, format!("Cut {removed} blocks"));
        } else {
            message(game, format!("Copied {copied} blocks"));
        }
        true
    }
    fn paste_at(
        game: &mut crate::game::Game<'_>,
        state: &mut State,
        at: Pos,
    ) -> usize {
        let blocks = std::mem::take(&mut state.clipboard);
        let mut ops = Vec::with_capacity(blocks.len());
        for (offset, block) in &blocks {
            let pos = [at[0] + offset[0], at[1] + offset[1], at[2] + offset[2]];
            if pos[1] < game.world.shape.min_y || pos[1] > game.world.shape.max_y() {
                continue;
            }
            if !game.world.loaded(pos[0], pos[2]) {
                continue;
            }
            ops.push((pos, *block));
        }
        let changed = place_ops(game, state, &ops);
        state.clipboard = blocks;
        changed
    }
    fn paste(game: &mut crate::game::Game<'_>, state: &mut State) -> bool {
        if state.clipboard.is_empty() {
            failure(
                game,
                "The clipboard is empty (//copy, //cut or //schem load first)",
            );
            return false;
        }
        let at = player_pos(game);
        let changed = paste_at(game, state, at);
        if state.clipboard_name.is_empty() {
            message(game, format!("Pasted {changed} blocks"));
        } else {
            message(
                game,
                format!("Pasted {changed} blocks of {}", state.clipboard_name),
            );
        }
        true
    }
    fn move_selection(
        game: &mut crate::game::Game<'_>,
        state: &mut State,
        args: &[&str],
    ) -> bool {
        let amount: i32 = args
            .first()
            .and_then(|a| a.parse::<i32>().ok())
            .unwrap_or(1)
            .abs()
            .max(1);
        let Some(word) = args.get(1) else {
            failure(
                game,
                "Usage: //move <amount> <up|down|north|south|east|west>",
            );
            return false;
        };
        let Some((axis, positive)) = worldedit::direction(word) else {
            failure(
                game,
                "Usage: //move <amount> <up|down|north|south|east|west>",
            );
            return false;
        };
        if !copy(game, state, false) {
            return false;
        }
        let mut at = player_pos(game);
        at[axis] = at[axis].saturating_add(if positive { amount } else { -amount });
        let changed = paste_at(game, state, at);
        message(game, format!("Moved {changed} blocks"));
        true
    }
    fn undo(game: &mut crate::game::Game<'_>, state: &mut State) -> bool {
        let Some(edit) = state.undo.pop() else {
            failure(game, "Nothing to undo");
            return false;
        };
        let mut redo = Vec::with_capacity(edit.len());
        for &(pos, old) in &edit {
            redo.push((pos, game.world.block(pos[0], pos[1], pos[2])));
            let _ = game.level_set(pos, old);
        }
        message(game, format!("Undid {} block changes", edit.len()));
        state.redo.push(redo);
        true
    }
    fn redo(game: &mut crate::game::Game<'_>, state: &mut State) -> bool {
        let Some(edit) = state.redo.pop() else {
            failure(game, "Nothing to redo");
            return false;
        };
        let mut undo = Vec::with_capacity(edit.len());
        for &(pos, block) in &edit {
            undo.push((pos, game.world.block(pos[0], pos[1], pos[2])));
            let _ = game.level_set(pos, block);
        }
        message(game, format!("Redid {} block changes", edit.len()));
        state.undo.push(undo);
        true
    }

    /// This world's schematics: `<world>/schematics`, where `//schem save`
    /// writes and `//schem load` looks first.
    fn schematics_dir(game: &crate::game::Game<'_>) -> PathBuf {
        // The window of a world on its own server thread has no folder of its
        // own: the world it opened is the one the helpers remember.
        let world = if game.world_root.as_os_str().is_empty() {
            worldedit::paths::remembered().unwrap_or_default()
        } else {
            game.world_root.clone()
        };
        if world.as_os_str().is_empty() {
            return PathBuf::new();
        }
        worldedit::paths::ensure_world(&world)
    }
    /// The file `name` means: a path as typed, else this world's
    /// `schematics/`.
    fn schematic_path(game: &crate::game::Game<'_>, name: &str) -> Option<PathBuf> {
        worldedit::schematic_path(name, &schematics_dir(game))
    }
    /// A block as a schematic names it:
    /// `minecraft:oak_stairs[facing=east,half=top]`.
    fn block_spec(game: &crate::game::Game<'_>, state: BlockStateId) -> String {
        let Some(descriptor) = game.pack.assets.registry.state(state) else {
            return "minecraft:air".to_owned();
        };
        if descriptor.properties.is_empty() {
            return descriptor.name.clone();
        }
        let pairs: Vec<String> = descriptor
            .properties
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        format!("{}[{}]", descriptor.name, pairs.join(","))
    }
    fn schem(
        game: &mut crate::game::Game<'_>,
        state: &mut State,
        args: &[&str],
    ) -> bool {
        match args
            .first()
            .map(|word| word.to_ascii_lowercase())
            .as_deref()
        {
            Some("list") => list_schematics(game),
            Some("save") => save_schematic(game, state, &args[1..]),
            Some("load") => load_schematic(game, state, &args[1..]),
            Some("delete") | Some("remove") => delete_schematic(game, &args[1..]),
            Some("folder") | Some("open") => folder(game),
            Some(_) => load_schematic(game, state, args),
            // A window browses the folder; a console only has the list.
            None => {
                if menu_open(game) {
                    return true;
                }
                list_schematics(game)
            }
        }
    }
    /// `//schem folder`: the world's schematics folder, in the file manager,
    /// so files can be dragged into it.
    fn folder(game: &mut crate::game::Game<'_>) -> bool {
        let dir = schematics_dir(game);
        if dir.as_os_str().is_empty() {
            failure(
                game,
                "No world folder yet: schematics live beside the world",
            );
            return false;
        }
        crate::game::packs::open_folder(&dir);
        message(game, format!("Opened {}", dir.display()));
        true
    }

    /// The schematics menu, over the world this game owns: the same gate and
    /// opening the `handle_chat_input` helper makes (`//schem` typed in
    /// chat), because the two helpers cannot see each other. False when it
    /// has no window over a world of its own to open on.
    fn menu_open(game: &mut crate::game::Game<'_>) -> bool {
        if game.renderer.is_none() || !(game.integrated.is_some() || game.remote.is_none()) {
            return false;
        }
        let dir = if game.world_root.as_os_str().is_empty() {
            worldedit::paths::schematics()
        } else {
            Some(worldedit::paths::ensure_world(&game.world_root))
        };
        let Some(dir) = dir else {
            game.command_failure(horizon_client::text::Text::literal(
                "No world folder yet: //schem lists the schematics of the world you are in",
            ));
            return false;
        };
        worldedit::menu::open(worldedit::menu::Menu::at(dir));
        game.screen = crate::game::Screen::Paused;
        game.pause_menu = true;
        game.capture(false);
        true
    }

    // ---------------------------------------------------- schematic files
    //
    // Sponge `.schem`, Litematica `.litematic` and legacy MCEdit
    // `.schematic`, read and written beside the editor. A reader turns a file
    // into blocks through the name resolver the command hands it, and the
    // writer turns blocks into bytes.

    /// A schematic read from a file.
    struct Loaded {
        /// The format as a player knows it: `Sponge v2`, `Litematica v6`,
        /// `MCEdit`.
        format: String,
        /// The blocks' bounding box, in blocks.
        size: Pos,
        /// Blocks relative to the paste origin.
        blocks: Vec<(Pos, BlockStateId)>,
        /// Palette entries this game has no block for (left out, reported).
        unknown: usize,
    }

    /// The most blocks a schematic may hold: pasting is one edit, so the same
    /// cap as `//set` and the rest, and a file bigger than this is a mistake,
    /// not a build.
    const MAX_BLOCKS: usize = 250_000;

    /// Reads `path`: Sponge, Litematica or MCEdit, whichever the file is.
    fn read_schematic(
        path: &Path,
        resolve: &dyn Fn(&str) -> Option<BlockStateId>,
    ) -> Result<Loaded, String> {
        let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let raw = decompress(&bytes)?;
        let (named, _) = nbt::read_named(&raw, Limits::templates()).map_err(|e| e.to_string())?;
        let root = &named.tag;
        let loaded = if root.get("Regions").is_some() {
            read_litematic(root, resolve)?
        } else if matches!(root.get("Blocks"), Some(Tag::ByteArray(_))) {
            read_mcedit(root, resolve)?
        } else {
            read_sponge(root, resolve)?
        };
        Ok(loaded)
    }

    /// Gzip and zlib NBT, and bare NBT.
    fn decompress(bytes: &[u8]) -> Result<Vec<u8>, String> {
        let given = match bytes {
            [0x1f, 0x8b, ..] => horizon_world::compression::Compression::Gzip,
            [0x78, ..] => horizon_world::compression::Compression::Zlib,
            _ => horizon_world::compression::Compression::None,
        };
        given
            .decode(bytes, 1 << 28)
            .map_err(|error| format!("not a gzip, zlib or raw NBT file ({error})"))
    }

    fn int(tag: Option<&Tag>) -> Option<i64> {
        match tag? {
            Tag::Byte(value) => Some(i64::from(*value)),
            Tag::Short(value) => Some(i64::from(*value)),
            Tag::Int(value) => Some(i64::from(*value)),
            Tag::Long(value) => Some(*value),
            _ => None,
        }
    }

    fn text(tag: Option<&Tag>) -> Option<String> {
        match tag? {
            Tag::String(value) => Some(value.to_string_lossy()),
            _ => None,
        }
    }

    fn ints(tag: Option<&Tag>) -> Option<Vec<i32>> {
        match tag? {
            Tag::IntArray(values) => Some(values.clone()),
            Tag::List(values) => Some(
                values
                    .iter()
                    .filter_map(|value| int(Some(value)).map(|v| v as i32))
                    .collect(),
            ),
            Tag::ByteArray(values) => Some(values.iter().map(|v| i32::from(*v)).collect()),
            _ => None,
        }
    }

    fn position(tag: Option<&Tag>) -> Option<Pos> {
        let compound = match tag? {
            tag @ Tag::Compound(_) => tag,
            _ => return None,
        };
        Some([
            compound.get("x").and_then(|t| int(Some(t)))? as i32,
            compound.get("y").and_then(|t| int(Some(t)))? as i32,
            compound.get("z").and_then(|t| int(Some(t)))? as i32,
        ])
    }

    /// The size a file declares, checked against the limits of a paste.
    fn checked_size(width: i64, height: i64, length: i64) -> Result<Pos, String> {
        if width <= 0 || height <= 0 || length <= 0 {
            return Err("the file holds no blocks".to_owned());
        }
        let volume = (width as u128) * (height as u128) * (length as u128);
        if volume > MAX_BLOCKS as u128 {
            return Err(format!(
                "that schematic is {volume} blocks; the most this can load is {MAX_BLOCKS}"
            ));
        }
        Ok([width as i32, height as i32, length as i32])
    }

    /// `minecraft:oak_stairs[facing=east,half=top]` from a palette entry's
    /// `Name` and `Properties`.
    fn spec_of(tag: &Tag) -> Option<String> {
        let name = text(tag.get("Name"))?;
        let mut properties: Vec<(String, String)> = Vec::new();
        if let Some(Tag::Compound(map)) = tag.get("Properties") {
            for (key, value) in map {
                properties.push((key.to_string_lossy(), text(Some(value))?));
            }
        }
        properties.sort();
        if properties.is_empty() {
            return Some(name);
        }
        let pairs: Vec<String> = properties
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        Some(format!("{name}[{}]", pairs.join(",")))
    }

    /// Sponge, v1 to v3. v3 keeps the palette and the data under `Blocks`, and
    /// every version indexes `(y * length + z) * width + x` with varints.
    fn read_sponge(
        root: &Tag,
        resolve: &dyn Fn(&str) -> Option<BlockStateId>,
    ) -> Result<Loaded, String> {
        let body = match root.get("Schematic") {
            Some(tag @ Tag::Compound(_)) => tag,
            _ => root,
        };
        let version = int(body.get("Version")).unwrap_or(1);
        let width = int(body.get("Width")).ok_or("the schematic has no Width")?;
        let height = int(body.get("Height")).ok_or("the schematic has no Height")?;
        let length = int(body.get("Length")).ok_or("the schematic has no Length")?;
        let size = checked_size(width, height, length)?;
        let offset = ints(body.get("Offset"))
            .filter(|values| values.len() == 3)
            .unwrap_or_else(|| vec![0, 0, 0]);
        let inner = body.get("Blocks");
        let palette_tag = inner
            .and_then(|blocks| blocks.get("Palette"))
            .or_else(|| body.get("Palette"))
            .or_else(|| body.get("Palettes").and_then(|p| p.get("Default")))
            .ok_or("the schematic has no block palette")?;
        let data = match inner.and_then(|blocks| blocks.get("Data")) {
            Some(tag) => tag,
            None => body
                .get("BlockData")
                .ok_or("the schematic has no block data")?,
        };
        let Tag::ByteArray(data) = data else {
            return Err("the block data is not a byte array".to_owned());
        };
        let palette = palette_entries(palette_tag, resolve)?;
        let indices = varints(data);
        let volume = (size[0] as usize) * (size[1] as usize) * (size[2] as usize);
        if indices.len() < volume {
            return Err(format!(
                "the schematic holds {} blocks, not {volume}",
                indices.len()
            ));
        }
        let mut blocks = Vec::with_capacity(volume);
        let mut unknown = 0usize;
        for y in 0..size[1] {
            for z in 0..size[2] {
                for x in 0..size[0] {
                    let index = ((y as usize * size[2] as usize) + z as usize)
                        * size[0] as usize
                        + x as usize;
                    match palette.get(indices[index] as usize).copied().flatten() {
                        Some(state) => blocks.push((
                            [x - offset[0], y - offset[1], z - offset[2]],
                            state,
                        )),
                        None => unknown += 1,
                    }
                }
            }
        }
        Ok(Loaded {
            format: format!("Sponge v{version}"),
            size,
            blocks,
            unknown,
        })
    }

    /// A Sponge palette: index to state, unknown names kept as `None`.
    fn palette_entries(
        tag: &Tag,
        resolve: &dyn Fn(&str) -> Option<BlockStateId>,
    ) -> Result<Vec<Option<BlockStateId>>, String> {
        let Tag::Compound(map) = tag else {
            return Err("the block palette is not a compound".to_owned());
        };
        let mut entries: Vec<(i64, Option<BlockStateId>)> = Vec::with_capacity(map.len());
        for (name, value) in map {
            let index = int(Some(value)).ok_or("a palette index is not a number")?;
            entries.push((index, resolve(&name.to_string_lossy())));
        }
        let highest = entries.iter().map(|(index, _)| *index).max().unwrap_or(-1);
        if highest < 0 || highest as usize > 1 << 20 {
            return Err("the block palette is empty".to_owned());
        }
        let mut out = vec![None; highest as usize + 1];
        for (index, state) in entries {
            out[index as usize] = state;
        }
        Ok(out)
    }

    /// Minecraft's varints, as Sponge's block data uses them.
    fn varints(bytes: &[u8]) -> Vec<u32> {
        let mut out = Vec::new();
        let mut value = 0u32;
        let mut shift = 0u32;
        for &byte in bytes {
            value |= u32::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                out.push(value);
                value = 0;
                shift = 0;
            } else {
                shift += 7;
                if shift > 28 {
                    value = 0;
                    shift = 0;
                }
            }
        }
        out
    }

    fn put_varint(out: &mut Vec<u8>, mut value: u32) {
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                out.push(byte);
                return;
            }
            out.push(byte | 0x80);
        }
    }

    /// Litematica: one compound per region, each with its own palette and a
    /// packed `BlockStates` array. `Size` may be negative, for a mirrored
    /// region.
    fn read_litematic(
        root: &Tag,
        resolve: &dyn Fn(&str) -> Option<BlockStateId>,
    ) -> Result<Loaded, String> {
        let Tag::Compound(regions) = root
            .get("Regions")
            .ok_or("the litematic has no Regions")?
        else {
            return Err("the litematic's Regions is not a compound".to_owned());
        };
        if regions.is_empty() {
            return Err("the litematic holds no regions".to_owned());
        }
        let mut absolute: Vec<(Pos, BlockStateId)> = Vec::new();
        let mut unknown = 0usize;
        for (_, region) in regions {
            let start = position(region.get("Position")).ok_or("a region has no Position")?;
            let signed = position(region.get("Size")).ok_or("a region has no Size")?;
            let span = [signed[0].abs(), signed[1].abs(), signed[2].abs()];
            if span.iter().any(|side| *side == 0) {
                continue;
            }
            let Tag::List(palette) = region
                .get("BlockStatePalette")
                .ok_or("a region has no BlockStatePalette")?
            else {
                return Err("a region's BlockStatePalette is not a list".to_owned());
            };
            let states: Vec<Option<BlockStateId>> = palette
                .iter()
                .map(|entry| spec_of(entry).and_then(|spec| resolve(&spec)))
                .collect();
            if states.is_empty() {
                return Err("a region's palette is empty".to_owned());
            }
            let volume = (span[0] as usize) * (span[1] as usize) * (span[2] as usize);
            if volume > MAX_BLOCKS {
                return Err(format!(
                    "that region is {volume} blocks; the most this can load is {MAX_BLOCKS}"
                ));
            }
            let longs: Vec<i64> = match region.get("BlockStates") {
                Some(Tag::LongArray(values)) => values.clone(),
                Some(Tag::List(values)) => values
                    .iter()
                    .filter_map(|value| match value {
                        Tag::Long(v) => Some(*v),
                        Tag::Int(v) => Some(i64::from(*v)),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            let values = if longs.is_empty() && states.len() == 1 {
                vec![0u32; volume]
            } else {
                unpack(&longs, bits_for(states.len()), volume).ok_or_else(|| {
                    format!(
                        "a region's BlockStates holds {} longs, too few for {volume} blocks",
                        longs.len()
                    )
                })?
            };
            for y in 0..span[1] {
                for z in 0..span[2] {
                    for x in 0..span[0] {
                        let index = ((y as usize * span[2] as usize) + z as usize)
                            * span[0] as usize
                            + x as usize;
                        let Some(state) = states
                            .get(values[index] as usize)
                            .copied()
                            .flatten()
                        else {
                            unknown += 1;
                            continue;
                        };
                        let local = [
                            if signed[0] < 0 { -x } else { x },
                            if signed[1] < 0 { -y } else { y },
                            if signed[2] < 0 { -z } else { z },
                        ];
                        absolute.push((
                            [
                                start[0] + local[0],
                                start[1] + local[1],
                                start[2] + local[2],
                            ],
                            state,
                        ));
                    }
                }
            }
        }
        if absolute.is_empty() {
            return Err("the litematic holds no blocks".to_owned());
        }
        let (size, blocks) = normalize(absolute);
        let version = int(root.get("Version")).unwrap_or(0);
        Ok(Loaded {
            format: if version > 0 {
                format!("Litematica v{version}")
            } else {
                "Litematica".to_owned()
            },
            size,
            blocks,
            unknown,
        })
    }

    /// Litematica's bit width for a palette: at least two bits, otherwise the
    /// bits its highest index needs (`Math.max(2, Bits.calcBits(palette))`).
    fn bits_for(palette: usize) -> u32 {
        let highest = palette.max(1) - 1;
        let needed = usize::BITS - (highest | 1).leading_zeros();
        needed.clamp(2, 32)
    }

    /// Values packed across `long` boundaries, least significant bit first, as
    /// Litematica and 1.16+ palettes store them.
    fn unpack(longs: &[i64], bits: u32, count: usize) -> Option<Vec<u32>> {
        if bits == 0 || bits > 32 {
            return None;
        }
        let needed = (count as u128 * bits as u128).div_ceil(64) as usize;
        if longs.len() < needed {
            return None;
        }
        let mask = if bits == 32 {
            u64::from(u32::MAX)
        } else {
            (1u64 << bits) - 1
        };
        let mut out = Vec::with_capacity(count);
        for index in 0..count {
            let start = index as u64 * u64::from(bits);
            let slot = (start / 64) as usize;
            let shift = start % 64;
            let mut value = (longs[slot] as u64) >> shift;
            if shift + u64::from(bits) > 64 {
                let next = longs.get(slot + 1).copied().unwrap_or(0) as u64;
                value |= next << (64 - shift);
            }
            out.push((value & mask) as u32);
        }
        Some(out)
    }

    /// MCEdit and WorldEdit's legacy `.schematic`: one byte per block from
    /// 1.12's numeric ids, nibbles for the data (and the high id bits, when
    /// `AddBlocks` holds them).
    fn read_mcedit(
        root: &Tag,
        resolve: &dyn Fn(&str) -> Option<BlockStateId>,
    ) -> Result<Loaded, String> {
        let width = int(root.get("Width")).ok_or("the schematic has no Width")?;
        let height = int(root.get("Height")).ok_or("the schematic has no Height")?;
        let length = int(root.get("Length")).ok_or("the schematic has no Length")?;
        let size = checked_size(width, height, length)?;
        let Some(Tag::ByteArray(ids)) = root.get("Blocks") else {
            return Err("the schematic has no Blocks".to_owned());
        };
        let data: &[u8] = match root.get("Data") {
            Some(Tag::ByteArray(values)) => values,
            _ => &[],
        };
        let added: &[u8] = match root.get("AddBlocks") {
            Some(Tag::ByteArray(values)) => values,
            _ => &[],
        };
        let offset = ints(root.get("WEOffset"))
            .or_else(|| ints(root.get("Offset")))
            .filter(|values| values.len() == 3)
            .unwrap_or_else(|| vec![0, 0, 0]);
        let volume = (size[0] as usize) * (size[1] as usize) * (size[2] as usize);
        if ids.len() < volume {
            return Err(format!(
                "the schematic holds {} blocks, not {volume}",
                ids.len()
            ));
        }
        let mut blocks = Vec::with_capacity(volume);
        let mut unknown = 0usize;
        for y in 0..size[1] {
            for z in 0..size[2] {
                for x in 0..size[0] {
                    let index = ((y as usize * size[2] as usize) + z as usize)
                        * size[0] as usize
                        + x as usize;
                    let mut id = i32::from(ids[index]);
                    if !added.is_empty() {
                        let byte = added[index / 2];
                        let high = if index % 2 == 0 { byte & 0x0f } else { byte >> 4 };
                        id |= i32::from(high) << 8;
                    }
                    let nibble = match data.get(index / 2) {
                        Some(byte) => {
                            if index % 2 == 0 {
                                byte & 0x0f
                            } else {
                                byte >> 4
                            }
                        }
                        None => 0,
                    };
                    let legacy = (id << 4) | i32::from(nibble);
                    if !(0..4096).contains(&legacy) {
                        unknown += 1;
                        continue;
                    }
                    let (name, properties) = worldedit::legacy::state(legacy as usize);
                    let spec = if properties.is_empty() {
                        name.to_owned()
                    } else {
                        format!("{name}[{}]", properties.replace(';', ","))
                    };
                    match resolve(&spec) {
                        Some(state) => blocks.push((
                            [x - offset[0], y - offset[1], z - offset[2]],
                            state,
                        )),
                        None => unknown += 1,
                    }
                }
            }
        }
        Ok(Loaded {
            format: "MCEdit".to_owned(),
            size,
            blocks,
            unknown,
        })
    }

    /// Shifts blocks so their low corner is at `0, 0, 0`, and reports the box.
    fn normalize(blocks: Vec<(Pos, BlockStateId)>) -> (Pos, Vec<(Pos, BlockStateId)>) {
        let mut low = [i32::MAX; 3];
        let mut high = [i32::MIN; 3];
        for (pos, _) in &blocks {
            for axis in 0..3 {
                low[axis] = low[axis].min(pos[axis]);
                high[axis] = high[axis].max(pos[axis]);
            }
        }
        let shifted = blocks
            .into_iter()
            .map(|(pos, state)| {
                (
                    [pos[0] - low[0], pos[1] - low[1], pos[2] - low[2]],
                    state,
                )
            })
            .collect();
        (
            [
                high[0] - low[0] + 1,
                high[1] - low[1] + 1,
                high[2] - low[2] + 1,
            ],
            shifted,
        )
    }

    /// A Sponge v2 `.schem` from `order`, which holds the blocks in Sponge's
    /// index order (`(y * length + z) * width + x`), each named by `describe`.
    fn write_sponge(
        size: Pos,
        order: &[BlockStateId],
        describe: &dyn Fn(BlockStateId) -> String,
    ) -> Result<Vec<u8>, String> {
        if order.len() != (size[0] as usize) * (size[1] as usize) * (size[2] as usize) {
            return Err("the selection and its blocks disagree".to_owned());
        }
        let mut palette: Vec<(String, i32)> = Vec::new();
        let mut index_of: HashMap<String, i32> = HashMap::new();
        let mut data = Vec::with_capacity(order.len());
        for &state in order {
            let spec = describe(state);
            let index = match index_of.get(&spec) {
                Some(&index) => index,
                None => {
                    let index = palette.len() as i32;
                    index_of.insert(spec.clone(), index);
                    palette.push((spec, index));
                    index
                }
            };
            put_varint(&mut data, index as u32);
        }
        let entries: Vec<(&str, Tag)> = palette
            .iter()
            .map(|(spec, index)| (spec.as_str(), Tag::Int(*index)))
            .collect();
        let body = Tag::compound([
            ("Version", Tag::Int(2)),
            ("DataVersion", Tag::Int(horizon_world::DATA_VERSION)),
            ("Width", Tag::Short(size[0] as i16)),
            ("Height", Tag::Short(size[1] as i16)),
            ("Length", Tag::Short(size[2] as i16)),
            ("Offset", Tag::IntArray(vec![0, 0, 0])),
            ("PaletteMax", Tag::Int(palette.len() as i32)),
            ("Palette", Tag::compound(entries)),
            ("BlockData", Tag::ByteArray(data)),
        ]);
        let named = NamedTag {
            name: "Schematic".into(),
            tag: body,
        };
        let bytes = nbt::write_named(&named, Limits::templates()).map_err(|e| e.to_string())?;
        horizon_world::compression::Compression::Gzip
            .encode(&bytes)
            .map_err(|error| error.to_string())
    }

    fn load_schematic(
        game: &mut crate::game::Game<'_>,
        state: &mut State,
        args: &[&str],
    ) -> bool {
        if args.is_empty() {
            failure(
                game,
                format!(
                    "Usage: //schem load <name>, files in {}",
                    schematics_dir(game).display()
                ),
            );
            return false;
        }
        let name = args.join(" ");
        let found = schematic_path(game, &name).or_else(|| schematic_path(game, args[0]));
        let Some(path) = found else {
            failure(
                game,
                format!("No schematic {name} in {}", schematics_dir(game).display()),
            );
            return false;
        };
        let read = {
            let view: &crate::game::Game<'_> = game;
            read_schematic(&path, &|spec: &str| block_state(view, spec))
        };
        let loaded = match read {
            Ok(loaded) => loaded,
            Err(error) => {
                failure(game, format!("Could not read {}: {error}", path.display()));
                return false;
            }
        };
        let label = path
            .file_name()
            .map(|file| file.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let (size, count, unknown) = (loaded.size, loaded.blocks.len(), loaded.unknown);
        let format = loaded.format;
        state.clipboard = loaded.blocks;
        state.clipboard_name = format!("{label} ({format})");
        message(
            game,
            format!(
                "Loaded {label} ({format}): {}x{}x{}, {count} blocks; //paste places it at your feet",
                size[0], size[1], size[2]
            ),
        );
        if unknown > 0 {
            message(
                game,
                format!(
                    "  {unknown} of its blocks have no match in this game and were left out"
                ),
            );
        }
        true
    }
    fn save_schematic(
        game: &mut crate::game::Game<'_>,
        state: &mut State,
        args: &[&str],
    ) -> bool {
        let Some((lo, hi, volume)) = selection(game, state) else {
            return false;
        };
        if volume > worldedit::MAX_VOLUME {
            too_big(game, volume);
            return false;
        }
        let given = args.join(" ");
        let given = given.trim();
        let name = if given.is_empty() {
            let seconds = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs());
            format!("build-{seconds}")
        } else {
            given.to_owned()
        };
        if name.contains('/') || name.contains('\\') || name.contains(':') {
            failure(
                game,
                "A schematic's name cannot hold a path: //schem save <name>",
            );
            return false;
        }
        let file = if name.to_ascii_lowercase().ends_with(".schem") {
            name.clone()
        } else {
            format!("{name}.schem")
        };
        let directory = schematics_dir(game);
        if let Err(error) = std::fs::create_dir_all(&directory) {
            failure(
                game,
                format!("Could not make {}: {error}", directory.display()),
            );
            return false;
        }
        let size = [hi[0] - lo[0] + 1, hi[1] - lo[1] + 1, hi[2] - lo[2] + 1];
        let air = air(game);
        let mut order = Vec::with_capacity(volume as usize);
        for y in lo[1]..=hi[1] {
            for z in lo[2]..=hi[2] {
                for x in lo[0]..=hi[0] {
                    order.push(if game.world.loaded(x, z) {
                        game.world.block(x, y, z)
                    } else {
                        air
                    });
                }
            }
        }
        let written = {
            let view: &crate::game::Game<'_> = game;
            write_sponge(size, &order, &|state: BlockStateId| block_spec(view, state))
        };
        let bytes = match written {
            Ok(bytes) => bytes,
            Err(error) => {
                failure(game, format!("Could not write {file}: {error}"));
                return false;
            }
        };
        let path = directory.join(&file);
        if let Err(error) = std::fs::write(&path, &bytes) {
            failure(game, format!("Could not write {}: {error}", path.display()));
            return false;
        }
        message(
            game,
            format!(
                "Saved {}x{}x{} ({} blocks) as {}",
                size[0],
                size[1],
                size[2],
                order.len(),
                path.display()
            ),
        );
        true
    }
    fn list_schematics(game: &mut crate::game::Game<'_>) -> bool {
        let directory = schematics_dir(game);
        if directory.as_os_str().is_empty() {
            message(
                game,
                "No world is open: schematics live in the world's own folder",
            );
            return true;
        }
        let entries = worldedit::paths::entries(&directory);
        message(
            game,
            format!(
                "Schematics in {} ({} file{}):",
                directory.display(),
                entries.len(),
                if entries.len() == 1 { "" } else { "s" }
            ),
        );
        if entries.is_empty() {
            message(
                game,
                "  (none: //schem save <name> makes one, or copy a .schem, .litematic or .schematic there)",
            );
            return true;
        }
        for entry in entries.iter().take(20) {
            message(
                game,
                format!(
                    "  {} ({} · {})",
                    entry.file,
                    entry.format,
                    worldedit::paths::human_size(entry.bytes)
                ),
            );
        }
        if entries.len() > 20 {
            message(game, format!("  ... and {} more", entries.len() - 20));
        }
        true
    }
    fn delete_schematic(game: &mut crate::game::Game<'_>, args: &[&str]) -> bool {
        if args.is_empty() {
            failure(game, "Usage: //schem delete <name>");
            return false;
        }
        let name = args.join(" ");
        let Some(path) = schematic_path(game, &name) else {
            failure(game, format!("No schematic {name}"));
            return false;
        };
        match std::fs::remove_file(&path) {
            Ok(()) => message(game, format!("Deleted {}", path.display())),
            Err(error) => {
                failure(
                    game,
                    format!("Could not delete {}: {error}", path.display()),
                );
                return false;
            }
        }
        true
    }

    // The hook itself.
    let Some((name, args)) = parse(command) else {
        return core::ops::ControlFlow::Continue(());
    };
    core::ops::ControlFlow::Break(run(game, &name, &args))
}
