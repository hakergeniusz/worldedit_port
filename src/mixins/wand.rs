// A mouse button while playing (`Game::play_mouse`), before the game starts
// mining or using an item: left click with the wooden axe `//wand` gave sets
// position 1, right click position 2, each at the block under the crosshair.
// The click is taken — no mining, no item use — and any other wooden axe
// stays an ordinary axe. The wand is known by the plain text of its custom
// name, read through the game's own derivation, so any shape the component
// is kept in (a bare string or a styled component) reads the same.
// Copyright (c) 2026 hakergeniusz. MIT.
fn worldedit_wand(
    game: &mut crate::game::Game<'_>,
    button: winit::event::MouseButton,
    pressed: bool,
) -> core::ops::ControlFlow<Result<()>> {
    /// The wand's mark on the held stack: the name `//wand` gives it.
    fn is_the_wand(stack: &horizon_client::items::ItemStack) -> bool {
        worldedit::is_wand(&stack.item, stack.custom_name().as_deref())
    }
    /// Whether the held stack is the wand `//wand` gave.
    fn holding(game: &crate::game::Game<'_>) -> bool {
        game.inventory
            .get(game.selected)
            .and_then(|slot| slot.as_ref())
            .is_some_and(is_the_wand)
    }

    if !pressed {
        return core::ops::ControlFlow::Continue(());
    }
    let name = match button {
        winit::event::MouseButton::Left => "pos1",
        winit::event::MouseButton::Right => "pos2",
        _ => return core::ops::ControlFlow::Continue(()),
    };
    if !holding(game) {
        return core::ops::ControlFlow::Continue(());
    }
    let Some(hit) = game.pick() else {
        return core::ops::ControlFlow::Continue(());
    };
    game.handle_chat_input(&format!(
        "//{name} {} {} {}",
        hit.pos[0], hit.pos[1], hit.pos[2]
    ));
    core::ops::ControlFlow::Break(Ok(()))
}
