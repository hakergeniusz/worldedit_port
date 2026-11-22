// WorldEdit's brain: pure standard library, no game types, so it compiles and
// tests wherever — as this crate's library, which every helper reaches as
// `worldedit::`. Every decision the helpers act on lives here, and every one
// of those decisions is tested below.
//
// The editor's shape: commands typed as `//name` (the slashes optional at the
// seams — the chat input, the console and the guest command packet each drop a
// different number of them) run on the game that owns the world, writing
// blocks through the game's own level path. What stays here is the part that
// touches no game state: the command table and its parse, the wand's mark,
// pattern and mask splitting, the region math, the salt that keeps a weighted
// pattern stable, and the three plain-data modules beside it —
//
//   paths   the schematics folder: where it is, what is in it
//   legacy  the 1.12 to 1.13 block table a legacy `.schematic` needs
//   menu    the schematics menu's list, layout, hit testing and open slot
//
// The helpers (src/mixins) hold the game side: reading the world, writing it,
// the registry, the messages, and the schematics menu's drawing.
//
// Clean-room: written for Horizon from the game's own public behavior and the
// original's feature description; it copies nothing from any mod.
//
// Copyright (c) 2026 hakergeniusz. MIT.

pub mod legacy;
pub mod menu;
pub mod paths;

/// The mod's registry id: the settings folder and the Options > Mods row.
pub const ID: &str = "worldedit";
/// The version the row shows.
pub const VERSION: &str = "1.3.0";

/// A block position: `x`, `y`, `z`.
pub type Pos = [i32; 3];

/// The most blocks one operation may touch, as WorldEdit's default limit.
pub const MAX_VOLUME: i64 = 250_000;
/// How many edits `//undo` remembers.
pub const MAX_HISTORY: usize = 10;

/// The wand: a wooden axe named `WorldEdit Wand`. The name is the mark, since
/// it is the one component that survives the item sync to the client, where
/// the click is handled; an ordinary wooden axe stays an axe.
pub const WAND_ITEM: &str = "minecraft:wooden_axe";
pub const WAND_NAME: &str = "WorldEdit Wand";

/// Whether `item` (its id) named `custom_name` (the stack's own, when it has
/// one) is the wand `//wand` handed out.
pub fn is_wand(item: &str, custom_name: Option<&str>) -> bool {
    item == WAND_ITEM && custom_name == Some(WAND_NAME)
}

/// The commands this mod owns. Nothing else is taken from the game.
pub const COMMANDS: &[&str] = &[
    "we",
    "wand",
    "pos1",
    "pos2",
    "sel",
    "expand",
    "contract",
    "shift",
    "set",
    "replace",
    "walls",
    "outline",
    "hollow",
    "faces",
    "overlay",
    "center",
    "count",
    "distr",
    "sphere",
    "hsphere",
    "cyl",
    "hcyl",
    "pyramid",
    "hpyramid",
    "line",
    "copy",
    "cut",
    "paste",
    "move",
    "undo",
    "redo",
    "clearhistory",
    "schem",
    "load",
];

/// `//we`'s lines, in the order the help message lists them.
pub const HELP: &[&str] = &[
    "WorldEdit (native). //wand gives a wooden axe: left click sets position 1, right click position 2.",
    "//pos1, //pos2 [x y z] (~ for where you stand)  //sel [clear]",
    "//expand, //contract, //shift <amount> [up|down|north|south|east|west]",
    "//set <block>[,<block>], //replace <from>[,<from>] [to], //walls, //outline, //hollow, //faces, //overlay, //center",
    "Patterns: //set stone,dirt or //set 70%stone,30%gravel picks a block per block, at random.",
    "//sphere, //hsphere, //cyl, //hcyl, //pyramid, //hpyramid <block> <radius> [height]",
    "//line <block> [thickness], //copy, //cut, //paste, //move <amount> <dir>",
    "//count <block>, //distr, //undo, //redo, //clearhistory",
    "//schem opens the schematics menu: pick a file, Load & Paste, or Open Folder and drag one in.",
    "//schem list, //schem load <name>, //schem save <name>, //schem delete <name>, //schem folder.",
    "Schematics read .schem (Sponge), .litematic (Litematica) and .schematic (MCEdit).",
];

/// The command name of `//name ...`, or None when this mod does not own it.
/// The slashes are optional here: the chat input, the console and the guest
/// command packet each drop a different number of them before the command
/// reaches its seam, so only the name is matched. The helper at the seam adds
/// the one check this cannot make — whether vanilla owns the name — since it
/// needs the game's dispatcher.
pub fn parse_base(command: &str) -> Option<(String, Vec<&str>)> {
    let rest = command.trim_start_matches('/');
    let mut words = rest.split_whitespace();
    let name = words.next()?.to_ascii_lowercase();
    if !COMMANDS.contains(&name.as_str()) {
        return None;
    }
    Some((name, words.collect()))
}

/// Splits a pattern or mask on the commas outside `[...]` block properties, so
/// `oak_stairs[facing=east,half=top],stone` is two entries.
pub fn split_entries(spec: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (i, ch) in spec.char_indices() {
        match ch {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                out.push(spec[start..i].to_owned());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(spec[start..].to_owned());
    out
}

/// An entry's weight and its text: `70%stone` is 70 and `stone`, `stone` is 1.
pub fn weighted(part: &str) -> Option<(u32, &str)> {
    match part.split_once('%') {
        Some((weight, rest)) => Some((weight.trim().parse::<u32>().ok()?.max(1), rest.trim())),
        None => Some((1, part.trim())),
    }
}

/// `up`, `down`, `north`, `south`, `east`, `west` as (axis, positive): x, y, z.
pub fn direction(word: &str) -> Option<(usize, bool)> {
    Some(match word {
        "up" => (1, true),
        "down" => (1, false),
        "east" => (0, true),
        "west" => (0, false),
        "south" => (2, true),
        "north" => (2, false),
        _ => return None,
    })
}

/// The salt of the next edit, from the current one: one more, scrambled. A
/// pattern spreads differently each time, but the spread never depends on the
/// order a region is walked in.
pub fn next_salt(current: u64) -> u64 {
    let salt = current.wrapping_mul(0x5851_f42d_4c95_7f2d).wrapping_add(1);
    scramble(salt)
}

/// The scramble a salt goes through before it picks blocks.
fn scramble(salt: u64) -> u64 {
    let mut h = salt;
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
    h ^= h >> 29;
    h
}

/// A stable number for `pos` under `salt`, so a block's pick never depends on
/// the order the region is walked in.
pub fn mix(salt: u64, pos: Pos) -> u64 {
    let mut h = salt ^ 0xcbf2_9ce4_8422_2325;
    for axis in pos {
        h ^= u64::from(axis as u32);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
    h ^ (h >> 29)
}

/// The selection's low corner, high corner and volume, clipped to the build
/// height. None while a corner is missing or the region sits outside it.
pub fn region_of(
    pos1: Option<Pos>,
    pos2: Option<Pos>,
    min_y: i32,
    max_y: i32,
) -> Option<(Pos, Pos, i64)> {
    let (a, b) = (pos1?, pos2?);
    let lo = [a[0].min(b[0]), a[1].min(b[1]).max(min_y), a[2].min(b[2])];
    let hi = [a[0].max(b[0]), a[1].max(b[1]).min(max_y), a[2].max(b[2])];
    if hi[1] < lo[1] {
        return None;
    }
    let volume = (i64::from(hi[0] - lo[0]) + 1)
        * (i64::from(hi[1] - lo[1]) + 1)
        * (i64::from(hi[2] - lo[2]) + 1);
    Some((lo, hi, volume))
}

/// The extensions a schematic name is tried with, in order: as typed, then the
/// three editors' spellings.
pub const SCHEMATIC_EXTENSIONS: [&str; 4] = ["", ".schem", ".litematic", ".schematic"];

/// The file `name` means: a path as typed, else the schematics folder
/// `directory` the world keeps. The first candidate that is a file wins.
pub fn schematic_path(name: &str, directory: &std::path::Path) -> Option<std::path::PathBuf> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let typed = std::path::Path::new(name);
    let external = typed.is_absolute() || name.contains('/') || name.contains('\\');
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if external {
        for extension in SCHEMATIC_EXTENSIONS {
            candidates.push(std::path::PathBuf::from(format!("{name}{extension}")));
        }
    }
    for extension in SCHEMATIC_EXTENSIONS {
        candidates.push(directory.join(format!("{name}{extension}")));
    }
    candidates.into_iter().find(|path| path.is_file())
}

/// The mod's registration handle: the game instantiates it for the dispatch
/// registration. The editor answers only at its seams, so it registers
/// nothing.
#[derive(Default)]
pub struct Mod;

#[mod_api::horizon_mod]
impl mod_api::Hooks for Mod {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_of_the_table_parses_with_and_without_its_slashes() {
        for name in COMMANDS {
            for typed in [format!("//{name}"), format!("/{name}"), name.to_string()] {
                let (parsed, args) = parse_base(&typed).expect(typed.as_str());
                assert_eq!(parsed, *name);
                assert!(args.is_empty());
            }
        }
    }

    #[test]
    fn a_command_not_of_the_table_is_none() {
        assert_eq!(parse_base("//give @s stone"), None);
        assert_eq!(parse_base("help"), None);
        assert_eq!(parse_base(""), None);
    }

    #[test]
    fn a_parse_keeps_its_arguments_in_order_and_cares_no_how_they_case() {
        let (name, args) = parse_base("//SCHEM load My Castle ").unwrap();
        assert_eq!(name, "schem");
        assert_eq!(args, vec!["load", "My", "Castle"]);
    }

    #[test]
    fn the_help_names_every_command() {
        let help = HELP.join("\n");
        // The one name the old help never said is `//we` itself: the chat
        // completions were its discovery, and they are gone with the screen
        // that drew them.
        for name in COMMANDS {
            assert!(
                *name == "we" || help.contains(name),
                "the help never says {name}"
            );
        }
    }

    #[test]
    fn a_wand_is_a_wooden_axe_by_its_name_alone() {
        assert!(is_wand("minecraft:wooden_axe", Some("WorldEdit Wand")));
        assert!(!is_wand("minecraft:wooden_axe", Some("Ordinary axe")));
        assert!(!is_wand("minecraft:wooden_axe", None));
        assert!(!is_wand("minecraft:iron_axe", Some("WorldEdit Wand")));
    }

    #[test]
    fn splitting_keeps_a_blocks_own_commas_whole() {
        let spec = "oak_stairs[facing=east,half=top],stone, 70%gravel";
        assert_eq!(
            split_entries(spec),
            ["oak_stairs[facing=east,half=top]", "stone", " 70%gravel"]
        );
        assert_eq!(split_entries("stone"), ["stone"]);
    }

    #[test]
    fn weights_read_from_the_front_and_stay_at_least_one() {
        assert_eq!(weighted("70%stone"), Some((70, "stone")));
        assert_eq!(weighted("stone"), Some((1, "stone")));
        assert_eq!(weighted("0%stone"), Some((1, "stone")));
        assert_eq!(weighted("many%stone"), None);
    }

    #[test]
    fn directions_cover_the_six_axes_and_nothing_else() {
        assert_eq!(direction("up"), Some((1, true)));
        assert_eq!(direction("down"), Some((1, false)));
        assert_eq!(direction("east"), Some((0, true)));
        assert_eq!(direction("west"), Some((0, false)));
        assert_eq!(direction("south"), Some((2, true)));
        assert_eq!(direction("north"), Some((2, false)));
        assert_eq!(direction("northeast"), None);
    }

    #[test]
    fn a_region_sits_between_its_corners_inside_the_build_height() {
        let region = region_of(Some([1, 2, 3]), Some([4, 5, 6]), -64, 319).unwrap();
        assert_eq!(region.0, [1, 2, 3]);
        assert_eq!(region.1, [4, 5, 6]);
        assert_eq!(region.2, 64);
        let reversed = region_of(Some([4, 5, 6]), Some([1, 2, 3]), -64, 319).unwrap();
        assert_eq!(reversed.0, [1, 2, 3]);
        assert_eq!(reversed.1, [4, 5, 6]);
    }

    #[test]
    fn a_region_above_the_ceiling_is_none_and_one_corner_is_enough() {
        assert_eq!(
            region_of(Some([0, 400, 0]), Some([0, 500, 0]), -64, 319),
            None
        );
        assert_eq!(
            region_of(Some([0, 350, 0]), Some([3, 360, 3]), -64, 319),
            None
        );
        assert_eq!(region_of(None, Some([1, 1, 1]), -64, 319), None);
        assert_eq!(region_of(Some([0, 0, 0]), None, -64, 319), None);
    }

    #[test]
    fn a_clipped_region_measures_what_it_touches() {
        let region = region_of(Some([0, -100, 0]), Some([3, 2, 3]), -64, 319).unwrap();
        assert_eq!(region.0, [0, -64, 0]);
        assert_eq!(region.1, [3, 2, 3]);
        assert_eq!(region.2, 4 * 67 * 4);
    }

    #[test]
    fn a_salt_moves_forward_and_never_repeats_its_first_step() {
        let mut salt = 0u64;
        let mut seen = std::collections::HashSet::new();
        for _ in 0..100 {
            salt = next_salt(salt);
            assert!(seen.insert(salt), "the salt repeated {salt}");
        }
    }

    #[test]
    fn a_blocks_pick_depends_on_where_it_is_not_what_walked_first() {
        let a = mix(7, [10, 64, -3]);
        let b = mix(7, [10, 65, -3]);
        assert_ne!(a, b);
        assert_eq!(a, mix(7, [10, 64, -3]), "the same place, the same pick");
    }

    #[test]
    fn a_schematic_name_finds_its_file_in_the_folder_first() {
        let root = std::env::temp_dir().join(format!("horizon-load-{}", std::process::id()));
        let dir = paths::ensure_world(&root);
        std::fs::write(dir.join("castle.schem"), b"x").unwrap();
        // A plain name: the folder's spellings, in order.
        assert_eq!(
            schematic_path("castle", &dir),
            Some(dir.join("castle.schem")),
            "no bare castle file exists, so the .schem spelling wins"
        );
        assert_eq!(
            schematic_path("castle.schem", &dir),
            Some(dir.join("castle.schem"))
        );
        assert_eq!(schematic_path("castle.litematic", &dir), None);
        assert_eq!(schematic_path("", &dir), None);
        assert_eq!(schematic_path("missing", &dir), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_schematic_path_as_typed_stays_outside_the_folder() {
        let root = std::env::temp_dir().join(format!("horizon-load-typed-{}", std::process::id()));
        let dir = paths::ensure_world(&root);
        let file = root.join("elsewhere.litematic");
        std::fs::write(&file, b"x").unwrap();
        let typed = file.display().to_string();
        assert_eq!(schematic_path(&typed, &dir), Some(file));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
