// A key while a world is open (`Game::key_pressed`): while the schematics
// menu is up, Esc closes it, Enter loads the pick, the arrows and paging move
// through the list, Home and End jump, F5 re-reads the folder — and nothing
// else reaches the game while it has the screen. The pause menu keeps its own
// keys otherwise.
// Copyright (c) 2026 hakergeniusz. MIT.
fn worldedit_schem_key(
    game: &mut crate::game::Game<'_>,
    key: winit::keyboard::KeyCode,
) -> core::ops::ControlFlow<Result<()>> {
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
    /// Sets the pick, with a click when it moved.
    fn set_pick(game: &mut crate::game::Game<'_>, index: usize) {
        let moved = worldedit::menu::with(|menu| {
            if index >= menu.entries.len() || menu.selected == Some(index) {
                return false;
            }
            menu.selected = Some(index);
            true
        })
        .unwrap_or(false);
        if moved {
            game.audio.play_button_click();
        }
    }
    /// Moves the pick by `by` rows, keeping it inside the list.
    fn step(game: &mut crate::game::Game<'_>, by: i64) {
        let next = worldedit::menu::with(|menu| {
            let last = menu.entries.len().checked_sub(1)?;
            let from = menu.selected.unwrap_or(0) as i64;
            Some((from + by).clamp(0, last as i64) as usize)
        });
        if let Some(next) = next.flatten() {
            set_pick(game, next);
        }
    }

    if !worldedit::menu::is_open() {
        return core::ops::ControlFlow::Continue(());
    }
    let (width, height) = game.gui_size();
    let rows = worldedit::menu::Layout::of(width, height).rows;
    match key {
        winit::keyboard::KeyCode::Escape => close(game),
        winit::keyboard::KeyCode::Enter | winit::keyboard::KeyCode::NumpadEnter => {
            paste_picked(game)
        }
        winit::keyboard::KeyCode::ArrowDown => step(game, 1),
        winit::keyboard::KeyCode::ArrowUp => step(game, -1),
        winit::keyboard::KeyCode::PageDown => step(game, rows as i64),
        winit::keyboard::KeyCode::PageUp => step(game, -(rows as i64)),
        winit::keyboard::KeyCode::Home | winit::keyboard::KeyCode::End => {
            let last = worldedit::menu::with(|menu| menu.entries.len()).unwrap_or(0);
            set_pick(
                game,
                if key == winit::keyboard::KeyCode::Home {
                    0
                } else {
                    last.saturating_sub(1)
                },
            );
        }
        winit::keyboard::KeyCode::F5 => {
            worldedit::menu::with(|menu| menu.refresh());
        }
        _ => {}
    }
    core::ops::ControlFlow::Break(Ok(()))
}
