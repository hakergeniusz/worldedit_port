// A command from a guest of a hosted world, before the hub decides what it is
// (`Game::guest_command`): one of the editor's commands is queued as typed,
// so the server runs it with that guest's state swapped in (`as_guest`) and
// the editor writes the world the way any command of theirs would. Everything
// else is left to the hub's own order of business.
// Copyright (c) 2026 hakergeniusz. MIT.
fn worldedit_guest_command(
    game: &mut crate::game::Game<'_>,
    conn: u32,
    command: &str,
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

    if parse(command).is_none() {
        return core::ops::ControlFlow::Continue(());
    }
    let Some(host) = game.lan.as_mut() else {
        return core::ops::ControlFlow::Continue(());
    };
    let Some(permission) = host.hub.guest(conn).map(|g| g.permission) else {
        return core::ops::ControlFlow::Continue(());
    };
    let integrated = host.hub.config.integrated;
    // Queued as typed, so `run_command` sees `/set ...` and the editor runs
    // with this guest's state swapped in (`as_guest`).
    host.commands.push(crate::game::lan_actions::GuestCommand {
        conn,
        command: command.to_owned(),
        permission,
        integrated,
        quiet: false,
    });
    core::ops::ControlFlow::Break(())
}
