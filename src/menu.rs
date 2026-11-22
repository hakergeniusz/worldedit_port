// The schematics menu: `//schem` with no argument, over a world this game
// owns. Every decision — where the list sits, what a click lands on, which
// rows the scroll shows, which file is picked — is plain standard library
// here, and the open menu itself is stored here, so the four helpers that
// touch it (the chat input that opens it, the pause screen's draw, click and
// keys) all see the same one without seeing each other. The helpers do the
// drawing and the game's own actions around it.
//
// Copyright (c) 2026 hakergeniusz. MIT.

use crate::paths::{self, Entry};
use std::cell::RefCell;
use std::path::PathBuf;

/// The list's width and a row's height.
pub const WIDTH: f32 = 340.0;
pub const ROW: f32 = 12.0;
/// Where the list starts, and the room the buttons and their hint take below.
pub const TOP: f32 = 42.0;
pub const FOOTER: f32 = 52.0;
/// How far the first row sits inside the list's frame.
pub const ROW_INSET: f32 = 3.0;
/// The hint under the list.
pub const HINT: &str = "Arrows pick · Enter loads · Esc closes";

/// What the open menu holds.
#[derive(Default)]
pub struct Menu {
    /// The folder the list reads.
    pub dir: PathBuf,
    /// The folder's schematics, as [`paths::entries`] lists them.
    pub entries: Vec<Entry>,
    /// The picked row.
    pub selected: Option<usize>,
    /// The first row the list shows.
    pub scroll: usize,
}

impl Menu {
    /// A menu over `dir`, listing it right away.
    pub fn at(dir: PathBuf) -> Self {
        let mut menu = Menu {
            dir,
            ..Menu::default()
        };
        menu.refresh();
        menu
    }

    /// Re-reads the folder, keeping the pick when that file is still there.
    pub fn refresh(&mut self) {
        let kept = self
            .selected
            .and_then(|i| self.entries.get(i))
            .map(|entry| entry.file.clone());
        self.entries = paths::entries(&self.dir);
        self.selected = kept
            .and_then(|file| self.entries.iter().position(|entry| entry.file == file))
            .or_else(|| (!self.entries.is_empty()).then_some(0));
        self.scroll = 0;
    }

    /// Keeps the pick on screen, and the scroll within the list.
    pub fn settle(&mut self, rows: usize) {
        let last = self.entries.len().saturating_sub(rows.max(1));
        self.scroll = self.scroll.min(last);
        if let Some(selected) = self.selected {
            if selected < self.scroll {
                self.scroll = selected;
            }
            if selected >= self.scroll + rows.max(1) {
                self.scroll = selected + 1 - rows.max(1);
            }
        }
    }

    /// The picked file.
    pub fn picked(&self) -> Option<&Entry> {
        self.selected.and_then(|i| self.entries.get(i))
    }
}

thread_local! {
    /// The open menu. The game's window thread owns the screen: the helpers
    /// that open, draw, click and key the menu all run on it, one at a time.
    static OPEN: RefCell<Option<Menu>> = const { RefCell::new(None) };
}

/// The menu is up, over `menu`'s folder.
pub fn open(menu: Menu) {
    OPEN.with(|slot| *slot.borrow_mut() = Some(menu));
}

/// The menu is closed.
pub fn close() {
    OPEN.with(|slot| *slot.borrow_mut() = None);
}

/// Whether a menu is up.
pub fn is_open() -> bool {
    OPEN.with(|slot| slot.borrow().is_some())
}

/// Runs `run` with the open menu; None when none is up.
pub fn with<R>(run: impl FnOnce(&mut Menu) -> R) -> Option<R> {
    OPEN.with(|slot| {
        let mut slot = slot.borrow_mut();
        slot.as_mut().map(run)
    })
}

/// Where the menu draws, from the screen's size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    /// The list's left edge, centered.
    pub x: f32,
    /// The list's first row.
    pub top: f32,
    /// The list's last pixel.
    pub bottom: f32,
    /// How many rows the list shows.
    pub rows: usize,
    /// The buttons' top edge.
    pub buttons_y: f32,
}

impl Layout {
    pub fn of(width: f32, height: f32) -> Self {
        let bottom = height - FOOTER;
        Self {
            x: (width / 2.0).floor() - WIDTH / 2.0,
            top: TOP,
            bottom,
            rows: (((bottom - TOP) / ROW).floor().max(1.0)) as usize,
            buttons_y: height - 27.0,
        }
    }

    /// `Load & Paste`, `Open Folder`, `Done`: left edge, width, label.
    pub fn buttons(&self) -> [(f32, f32, &'static str); 3] {
        [
            (self.x, 100.0, "Load & Paste"),
            (self.x + 110.0, 130.0, "Open Folder"),
            (self.x + 250.0, 90.0, "Done"),
        ]
    }
}

/// The Refresh button: x, y, width, height, from the screen's width.
pub fn refresh_button(width: f32) -> (f32, f32, f32, f32) {
    (width - 74.0, 6.0, 68.0, 16.0)
}

/// What a click on the menu landed on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    /// A row of the list.
    Row(usize),
    /// `Load & Paste`.
    Paste,
    /// `Open Folder`.
    Folder,
    /// The list's own Refresh button.
    Refresh,
    /// `Done`.
    Done,
    /// The menu's own background: the pause menu must not act under it.
    Elsewhere,
}

/// What a click at `cursor` lands on.
pub fn hit(menu: &Menu, layout: &Layout, width: f32, cursor: (f32, f32)) -> Hit {
    for (x, w, what) in layout.buttons() {
        if hovered(cursor, x, layout.buttons_y, w, 20.0) {
            return match what {
                "Load & Paste" => Hit::Paste,
                "Open Folder" => Hit::Folder,
                _ => Hit::Done,
            };
        }
    }
    let (rx, ry, rw, rh) = refresh_button(width);
    if hovered(cursor, rx, ry, rw, rh) {
        return Hit::Refresh;
    }
    if hovered(
        cursor,
        layout.x,
        layout.top,
        WIDTH,
        layout.bottom - layout.top,
    ) {
        let row = (cursor.1 - layout.top - ROW_INSET) / ROW;
        if row >= 0.0 {
            let index = menu.scroll + row as usize;
            if index < menu.entries.len() {
                return Hit::Row(index);
            }
        }
    }
    Hit::Elsewhere
}

/// The buttons' hover test, as the game's own `ui::hovered`.
fn hovered(cursor: (f32, f32), x: f32, y: f32, w: f32, h: f32) -> bool {
    cursor.0 >= x && cursor.0 < x + w && cursor.1 >= y && cursor.1 < y + h
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A layout on a 427x240 window, the smallest the game fits its GUI to.
    fn layout() -> Layout {
        Layout::of(427.0, 240.0)
    }

    /// A folder of this test's own, holding nothing: the temp directory is
    /// shared, and a stray `.schem` in it would become a row.
    fn empty_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("horizon-menu-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_list_sits_centered_with_rows_below_the_title() {
        let layout = layout();
        assert_eq!(layout.x, 43.0); // (427 / 2).floor() - 170
        assert_eq!(layout.top, 42.0);
        assert_eq!(layout.bottom, 188.0); // 240 - 52
        assert_eq!(layout.rows, 12); // (188 - 42) / 12
        assert_eq!(layout.buttons_y, 213.0); // 240 - 27
    }

    #[test]
    fn the_buttons_hold_their_labels_and_spots() {
        let layout = layout();
        let buttons = layout.buttons();
        assert_eq!(buttons[0], (layout.x, 100.0, "Load & Paste"));
        assert_eq!(buttons[1], (layout.x + 110.0, 130.0, "Open Folder"));
        assert_eq!(buttons[2], (layout.x + 250.0, 90.0, "Done"));
    }

    #[test]
    fn a_click_lands_on_what_it_is_over() {
        let layout = layout();
        let menu = Menu::at(empty_dir("hit"));
        let (rx, ry, _, _) = refresh_button(427.0);
        assert_eq!(
            hit(&menu, &layout, 427.0, (rx + 2.0, ry + 2.0)),
            Hit::Refresh
        );
        assert_eq!(
            hit(
                &menu,
                &layout,
                427.0,
                (layout.x + 5.0, layout.buttons_y + 5.0)
            ),
            Hit::Paste
        );
        assert_eq!(
            hit(
                &menu,
                &layout,
                427.0,
                (layout.x + 260.0, layout.buttons_y + 5.0)
            ),
            Hit::Done
        );
        assert_eq!(
            hit(&menu, &layout, 427.0, (layout.x + 5.0, layout.top + 6.0)),
            Hit::Elsewhere,
            "no rows listed yet, so the list's own background is elsewhere"
        );
        assert_eq!(hit(&menu, &layout, 427.0, (2.0, 2.0)), Hit::Elsewhere);
    }

    #[test]
    fn a_click_picks_the_row_under_it_by_the_scroll() {
        let layout = layout();
        let mut menu = Menu::at(empty_dir("rows"));
        menu.entries = (0..20)
            .map(|i| Entry {
                file: format!("{i}.schem"),
                name: format!("{i}"),
                format: "Sponge",
                bytes: 1,
            })
            .collect();
        menu.selected = Some(3);
        // The third row sits three row-heights under the list's inset edge.
        let y = layout.top + ROW_INSET + 3.0 * ROW + 1.0;
        assert_eq!(hit(&menu, &layout, 427.0, (layout.x + 5.0, y)), Hit::Row(3));
        menu.scroll = 10;
        assert_eq!(
            hit(&menu, &layout, 427.0, (layout.x + 5.0, y)),
            Hit::Row(13)
        );
    }

    #[test]
    fn refreshing_keeps_the_pick_while_the_file_stays() {
        let root = std::env::temp_dir().join(format!("horizon-menu-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.schem"), b"x").unwrap();
        std::fs::write(root.join("b.schem"), b"x").unwrap();
        let mut menu = Menu::at(root.clone());
        assert_eq!(menu.selected, Some(0));
        menu.selected = Some(1);
        menu.refresh();
        assert_eq!(menu.selected, Some(1));
        assert_eq!(menu.scroll, 0);
        std::fs::remove_file(root.join("b.schem")).unwrap();
        menu.refresh();
        assert_eq!(
            menu.selected,
            Some(0),
            "the pick falls back to the first row"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn settling_keeps_the_pick_on_screen_and_the_scroll_inside() {
        let mut menu = Menu {
            entries: (0..20)
                .map(|i| Entry {
                    file: format!("{i}.schem"),
                    name: format!("{i}"),
                    format: "Sponge",
                    bytes: 1,
                })
                .collect(),
            ..Menu::default()
        };
        let rows = layout().rows;
        menu.settle(rows);
        assert_eq!(menu.scroll, 0);
        menu.selected = Some(15);
        menu.settle(rows);
        assert_eq!(
            menu.scroll,
            15 + 1 - rows,
            "the pick sat at the list's last row"
        );
        menu.scroll = 100;
        menu.selected = None;
        menu.settle(rows);
        assert!(menu.scroll <= 20 - rows, "the scroll never passes the list");
    }

    #[test]
    fn the_open_menu_is_the_one_with_the_later_calls() {
        let dir = empty_dir("open");
        let menu = Menu::at(dir.clone());
        assert!(!is_open());
        open(menu);
        assert!(is_open());
        let seen = with(|open| open.dir.display().to_string());
        assert_eq!(seen, Some(dir.display().to_string()));
        close();
        assert!(!is_open());
        assert_eq!(with(|menu: &mut Menu| menu.selected), None);
    }
}
