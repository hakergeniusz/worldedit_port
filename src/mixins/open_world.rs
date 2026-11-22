// Entering a world (`Game::open_world`): its schematics folder is made, so
// the menu never lists a folder that is not there and files can be dragged
// in right away, and the world is noted, so the folder helpers know where
// the game last was.
// Copyright (c) 2026 hakergeniusz. MIT.
fn worldedit_open_world(game: &mut crate::game::Game<'_>, folder: &str) {
    let _ = game;
    let world = std::env::current_dir()
        .unwrap_or_default()
        .join("saves")
        .join(folder);
    worldedit::paths::ensure_world(&world);
}
