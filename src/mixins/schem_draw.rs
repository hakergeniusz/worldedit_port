// The pause menu's draw (`Game::draw_pause_menu`): the schematics menu draws
// itself over the screen while one is open — the box, the folder it lists,
// the rows, the scrollbar and the three buttons — and the pause menu keeps
// its own draw otherwise. Read-only: the clicks and keys answer in their own
// helpers.
// Copyright (c) 2026 hakergeniusz. MIT.
fn worldedit_schem_draw(
    game: &mut crate::game::Game<'_>,
    batch: &mut horizon_client::hud::HudBatch,
) -> core::ops::ControlFlow<()> {
    /// `text` cut from the front with `…` until it fits `max` pixels.
    fn short_path(
        atlas: &horizon_client::hud::HudAtlas,
        line: &str,
        max: f32,
    ) -> String {
        if atlas.font.width(line) <= max {
            return line.to_string();
        }
        let chars: Vec<char> = line.chars().collect();
        let mut keep = chars.len();
        while keep > 1 {
            keep -= 1;
            let short: String = std::iter::once('…')
                .chain(chars[chars.len() - keep..].iter().copied())
                .collect();
            if atlas.font.width(&short) <= max {
                return short;
            }
        }
        "…".to_string()
    }
    fn refresh_button(width: f32) -> (f32, f32, f32, f32) {
        worldedit::menu::refresh_button(width)
    }

    if !worldedit::menu::is_open() {
        return core::ops::ControlFlow::Continue(());
    }
    let (width, height) = (batch.width, batch.height);
    let cursor = game.cursor;
    let layout = worldedit::menu::Layout::of(width, height);
    let atlas = &game.gui.atlas;
    let white = u32::MAX;
    let gray = 0xFFA0_A0A0;
    worldedit::menu::with(|menu| {
        menu.settle(layout.rows);
        horizon_client::ui::menu_background(batch, atlas, 0.0, 0.0, width, height, true);
        // The title, and the folder it lists.
        let title = horizon_client::text::Text::literal("Schematics");
        let tw = atlas.font.width_text(&title);
        batch.text_styled(
            atlas,
            &title,
            ((width - tw) / 2.0).floor(),
            10.0,
            white,
            true,
        );
        // The path, shortened from the front so a deep world folder still
        // fits and keeps its own name visible.
        horizon_client::ui::centered_text(
            batch,
            atlas,
            &horizon_client::text::Text::colored(
                short_path(atlas, &menu.dir.display().to_string(), width - 24.0),
                horizon_client::text::GRAY,
            ),
            0.0,
            24.0,
            width,
            white,
        );
        let (rx, ry, rw, rh) = refresh_button(width);
        horizon_client::ui::button(
            batch,
            atlas,
            rx,
            ry,
            rw,
            rh,
            &horizon_client::text::Text::literal("Refresh"),
            true,
            horizon_client::ui::hovered(cursor, rx, ry, rw, rh),
        );
        // The list's frame.
        batch.fill(
            atlas,
            layout.x,
            layout.top,
            worldedit::menu::WIDTH,
            layout.bottom - layout.top,
            0x8000_0000,
        );
        for (x, y, w, h) in [
            (layout.x, layout.top, worldedit::menu::WIDTH, 1.0),
            (layout.x, layout.bottom - 1.0, worldedit::menu::WIDTH, 1.0),
            (layout.x, layout.top, 1.0, layout.bottom - layout.top),
            (
                layout.x + worldedit::menu::WIDTH - 1.0,
                layout.top,
                1.0,
                layout.bottom - layout.top,
            ),
        ] {
            batch.fill(atlas, x, y, w, h, 0xFF50_5050);
        }
        if menu.entries.is_empty() {
            for (i, line) in [
                "This folder has no schematics yet.",
                "Drop a .schem, .litematic or .schematic",
                "file into it, then press Refresh.",
            ]
            .iter()
            .enumerate()
            {
                horizon_client::ui::centered_text(
                    batch,
                    atlas,
                    &horizon_client::text::Text::colored(*line, horizon_client::text::GRAY),
                    layout.x + 4.0,
                    layout.top + 14.0 + i as f32 * worldedit::menu::ROW,
                    worldedit::menu::WIDTH - 8.0,
                    white,
                );
            }
        }
        let last = (menu.scroll + layout.rows).min(menu.entries.len());
        for (visible, index) in (menu.scroll..last).enumerate() {
            let y = layout.top + worldedit::menu::ROW_INSET + visible as f32 * worldedit::menu::ROW;
            let entry = &menu.entries[index];
            if menu.selected == Some(index) {
                batch.fill(
                    atlas,
                    layout.x + 2.0,
                    y,
                    worldedit::menu::WIDTH - 4.0,
                    worldedit::menu::ROW,
                    0x40FF_FFFF,
                );
            } else if horizon_client::ui::hovered(
                cursor,
                layout.x,
                y,
                worldedit::menu::WIDTH,
                worldedit::menu::ROW,
            ) {
                batch.fill(
                    atlas,
                    layout.x + 2.0,
                    y,
                    worldedit::menu::WIDTH - 4.0,
                    worldedit::menu::ROW,
                    0x20FF_FFFF,
                );
            }
            // The whole file name, extension and all: two files can share a
            // stem (`harbor.litematic`, `harbor.schematic`).
            batch.text(atlas, &entry.file, layout.x + 6.0, y + 2.0, white, true);
            let meta = format!(
                "{} · {}",
                entry.format,
                worldedit::paths::human_size(entry.bytes)
            );
            let mw = atlas.font.width(&meta);
            batch.text(
                atlas,
                &meta,
                layout.x + worldedit::menu::WIDTH - 8.0 - mw,
                y + 2.0,
                gray,
                true,
            );
        }
        if menu.entries.len() > layout.rows {
            horizon_client::ui::scrollbar(
                batch,
                atlas,
                layout.x + worldedit::menu::WIDTH - 5.0,
                layout.top + 2.0,
                layout.bottom - 2.0,
                menu.entries.len() as f32 * worldedit::menu::ROW,
                menu.scroll as f32 * worldedit::menu::ROW,
            );
        }
        let hint = horizon_client::text::Text::colored(
            worldedit::menu::HINT,
            horizon_client::text::GRAY,
        );
        horizon_client::ui::centered_text(
            batch,
            atlas,
            &hint,
            layout.x,
            layout.buttons_y - 13.0,
            worldedit::menu::WIDTH,
            white,
        );
        for (x, w, label) in layout.buttons() {
            let active = !(label == "Load & Paste" && menu.selected.is_none());
            horizon_client::ui::button(
                batch,
                atlas,
                x,
                layout.buttons_y,
                w,
                20.0,
                &horizon_client::text::Text::literal(label),
                active,
                horizon_client::ui::hovered(cursor, x, layout.buttons_y, w, 20.0),
            );
        }
        true
    });
    core::ops::ControlFlow::Break(())
}
