// A click on the pause menu's screen (`Game::pause_click`): while the
// schematics menu is up the click is the menu's — a row picks its file, the
// buttons load and paste, open the folder, refresh and close — and the pause
// menu keeps its own click otherwise. The pause menu's buttons are not under
// the menu, so nothing of theirs acts under it.
// Copyright (c) 2026 hakergeniusz. MIT.
fn worldedit_schem_click(game: &mut crate::game::Game<'_>) -> core::ops::ControlFlow<Result<()>> {
    /// Closes the menu and gives the mouse back to the world.
    fn close(game: &mut crate::game::Game<'_>) {
        worldedit::menu::close();
        if game.screen == crate::game::Screen::Paused {
            game.screen = crate::game::Screen::Playing;
            game.capture(true);
        }
    }
    /// A command line, to the world's server (this window's own, or
    /// another's).
    fn send(game: &mut crate::game::Game<'_>, line: &str) {
        if game.remote.is_some() {
            let _ = game.send_chat(line);
        } else if let Some(command) = line.strip_prefix('/') {
            game.run_command(command);
        }
    }
    /// `Load & Paste`: the picked file into the clipboard, then where you
    /// stand.
    fn paste_picked(game: &mut crate::game::Game<'_>) {
        let entry = worldedit::menu::with(|menu| menu.picked().cloned()).flatten();
        let Some(entry) = entry else {
            return;
        };
        game.audio.play_button_click();
        close(game);
        // The server of this world owns the clipboard: send both commands to
        // it, in order, the way a typed line would go.
        send(game, &format!("//schem load {}", entry.file));
        send(game, "//paste");
    }

    if !worldedit::menu::is_open() {
        return core::ops::ControlFlow::Continue(());
    }
    let (width, height) = game.gui_size();
    let cursor = game.cursor;
    let layout = worldedit::menu::Layout::of(width, height);
    let hit = worldedit::menu::with(|menu| worldedit::menu::hit(menu, &layout, width, cursor))
        .unwrap_or(worldedit::menu::Hit::Elsewhere);
    match hit {
        worldedit::menu::Hit::Paste => paste_picked(game),
        worldedit::menu::Hit::Folder => {
            let dir = worldedit::menu::with(|menu| menu.dir.clone());
            if let Some(dir) = dir {
                game.audio.play_button_click();
                crate::game::packs::open_folder(&dir);
            }
        }
        worldedit::menu::Hit::Refresh => {
            game.audio.play_button_click();
            worldedit::menu::with(|menu| menu.refresh());
        }
        worldedit::menu::Hit::Done => {
            game.audio.play_button_click();
            close(game);
        }
        worldedit::menu::Hit::Row(index) => {
            game.audio.play_button_click();
            worldedit::menu::with(|menu| menu.selected = Some(index));
        }
        worldedit::menu::Hit::Elsewhere => {}
    }
    core::ops::ControlFlow::Break(Ok(()))
}
