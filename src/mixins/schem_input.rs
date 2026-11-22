// `//schem` typed in the chat screen, before the message goes anywhere
// (`Game::handle_chat_input`): a window over its own world opens the menu,
// and everything else is left on its way — a guest's world, `//schem list`,
// `//schem load <file>`.
// Copyright (c) 2026 hakergeniusz. MIT.
fn worldedit_schem_input(
    game: &mut crate::game::Game<'_>,
    message: &str,
) -> core::ops::ControlFlow<()> {
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

    /// Opens the menu over the world this game owns. False when it has none:
    /// a guest of someone else's world leaves `//schem` to that world's
    /// server, and a game without a window (a dedicated server's console) to
    /// the chat list. The same opening the `run_command` helper makes when
    /// `//schem` arrives through it, because the two helpers cannot see each
    /// other.
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

    if let Some((name, args)) = parse(message)
        && name == "schem"
        && args.is_empty()
        && menu_open(game)
    {
        return core::ops::ControlFlow::Break(());
    }
    core::ops::ControlFlow::Continue(())
}
