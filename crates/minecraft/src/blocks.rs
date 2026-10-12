//! Block states and how each is drawn: its shape (a full cube for most,
//! cheap approximations for the common others), whether it is opaque,
//! cut out or see-through, its colours and whether it glows.

use crate::colors::{self, Colors, Pattern};
use crate::nbt::{Compound, Tag};

/*┌─────────────────╮
| Block states    |
└─────────────────╯*/
//region Block states

/// A block and its properties, as `minecraft:oak_stairs[facing=east]`
/// writes them.  The name loses the `minecraft:` namespace; other
/// namespaces (mods) stay.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BlockState {
    pub name: String,
    pub props: Vec<(String, String)>,
}

impl BlockState {
    pub fn new(name: &str, props: &[(&str, &str)]) -> BlockState {
        BlockState {
            name: name.strip_prefix("minecraft:").unwrap_or(name).to_string(),
            props: props.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        }
    }

    pub fn air() -> BlockState {
        BlockState::new("air", &[])
    }

    /// Parses `name[key=value,...]`.
    pub fn parse(text: &str) -> BlockState {
        let text = text.trim();
        let (name, rest) = text.split_once('[').unwrap_or((text, ""));
        let props = rest
            .trim_end_matches(']')
            .split(',')
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            .take(64)
            .collect();
        let name = name.trim();
        BlockState { name: name.strip_prefix("minecraft:").unwrap_or(name).to_ascii_lowercase(), props }
    }

    /// A palette entry of a structure or Litematica file: `Name` and a
    /// `Properties` compound of strings.
    pub fn from_nbt(c: &Compound) -> Option<BlockState> {
        let name = c.str("Name")?;
        let mut state = BlockState::parse(name);
        if let Some(props) = c.compound("Properties") {
            for (k, v) in props.0.iter().take(64) {
                if let Tag::String(v) = v {
                    state.props.push((k.to_string(), v.to_string()));
                }
            }
        }
        Some(state)
    }

    pub fn prop(&self, key: &str) -> Option<&str> {
        self.props.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    fn is(&self, key: &str, value: &str) -> bool {
        self.prop(key) == Some(value)
    }
}

/// Blocks that are never drawn: the airs, markers and technical blocks.
pub fn invisible(name: &str) -> bool {
    matches!(
        name,
        "air"
            | "cave_air"
            | "void_air"
            | "structure_void"
            | "barrier"
            | "light"
            | "moving_piston"
            | "tripwire"
            | "string"
    ) || name.ends_with("_button")
}

//endregion

/*┌─────────────────╮
| Looks           |
└─────────────────╯*/
//region Looks

/// The six directions, in the order the mesher sweeps them: +X (east), -X
/// (west), +Y (up), -Y (down), +Z (south), -Z (north).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    East,
    West,
    Up,
    Down,
    South,
    North,
}

impl Dir {
    pub const ALL: [Dir; 6] = [Dir::East, Dir::West, Dir::Up, Dir::Down, Dir::South, Dir::North];

    /// The axis (0 x, 1 y, 2 z) and whether the direction is positive.
    pub fn axis(self) -> (usize, bool) {
        match self {
            Dir::East => (0, true),
            Dir::West => (0, false),
            Dir::Up => (1, true),
            Dir::Down => (1, false),
            Dir::South => (2, true),
            Dir::North => (2, false),
        }
    }

    fn horizontal(v: Option<&str>) -> Dir {
        match v {
            Some("south") => Dir::South,
            Some("east") => Dir::East,
            Some("west") => Dir::West,
            _ => Dir::North,
        }
    }
}

/// How a block's faces are drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Layer {
    Opaque,
    /// Clear where the pattern is: plants, cobwebs, cages.
    Cutout,
    /// See-through: glass, ice, water, slime.
    Translucent,
}

/// A block's shape, in sixteenths of a block.
#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    Air,
    Cube,
    /// Water and lava: cubes that hide the faces between them.
    Liquid,
    /// Boxes, each `[x0, y0, z0, x1, y1, z1]` in sixteenths.
    Boxes(Vec<[u8; 6]>),
    /// Two crossed quads, as flowers and grass are drawn.
    Cross,
    /// A post with arms to the sides it connects to: fences (`Fence`),
    /// walls (`Wall`) and panes and bars (`Pane`).  The sides come from
    /// the block's properties, or from its neighbours when it has none
    /// (old schematics).
    Connected(Connect, [Option<bool>; 4]),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Connect {
    Fence,
    Wall,
    Pane,
}

/// Everything the mesher needs to draw one block state.
#[derive(Clone, Debug)]
pub struct Look {
    pub shape: Shape,
    pub layer: Layer,
    pub colors: Colors,
    /// Opacity of see-through blocks.
    pub alpha: f32,
    /// How brightly it glows, 0 for not at all.
    pub glow: f32,
    /// The name, which see-through cubes compare to hide the faces
    /// between two of a kind (glass against glass, water against water).
    pub kind: String,
    /// Whether the colour table knew the name.
    pub known: bool,
}

impl Look {
    /// Whether this block hides every face of a neighbour that touches it:
    /// an opaque full cube.
    pub fn solid(&self) -> bool {
        self.shape == Shape::Cube && self.layer == Layer::Opaque
    }

    /// Whether a face of `self` (a cube) against `other` is hidden.
    pub fn hidden_by(&self, other: &Look) -> bool {
        other.solid()
            || (matches!(other.shape, Shape::Cube | Shape::Liquid)
                && other.layer != Layer::Opaque
                && other.kind == self.kind)
    }

    pub fn air() -> Look {
        Look {
            shape: Shape::Air,
            layer: Layer::Opaque,
            colors: colors::lookup("stone").0,
            alpha: 1.0,
            glow: 0.0,
            kind: "air".into(),
            known: true,
        }
    }

    /// How much a cell prefers to keep this block when several blocks
    /// share it in a thinned-out thumbnail: walls over glass over plants.
    pub fn weight(&self) -> u8 {
        match (&self.shape, self.layer) {
            (Shape::Air, _) => 0,
            (Shape::Cube, Layer::Opaque) => 4,
            (Shape::Cube | Shape::Liquid, _) => 2,
            (Shape::Cross, _) => 1,
            _ => 3,
        }
    }
}

/// How the block state is drawn.
pub fn look(state: &BlockState) -> Look {
    let name = state.name.as_str();
    if invisible(name) {
        return Look::air();
    }
    let (colors, known) = colors::lookup(name.rsplit(':').next().unwrap_or(name));
    let shape = shape(state, &colors);
    let plant = colors.side.1 == Pattern::Plant;
    let (layer, alpha) = if name == "water" || name == "bubble_column" {
        (Layer::Translucent, 0.6)
    } else if name.ends_with("stained_glass") || name.ends_with("stained_glass_pane") {
        (Layer::Translucent, 0.55)
    } else if name == "glass" || name == "glass_pane" || name == "beacon" {
        (Layer::Translucent, 0.35)
    } else if name == "tinted_glass" {
        (Layer::Translucent, 0.85)
    } else if matches!(name, "ice" | "frosted_ice" | "slime_block" | "honey_block" | "nether_portal") {
        (Layer::Translucent, 0.75)
    } else if plant || matches!(name, "spawner" | "trial_spawner" | "scaffolding" | "cobweb") {
        (Layer::Cutout, 1.0)
    } else {
        (Layer::Opaque, 1.0)
    };
    Look { shape, layer, colors, alpha, glow: glow(state), kind: name.to_string(), known }
}

/// How brightly a block glows, as a fraction of full light.
fn glow(state: &BlockState) -> f32 {
    let name = state.name.as_str();
    let lit = state.prop("lit") != Some("false");
    match name {
        "glowstone"
        | "sea_lantern"
        | "shroomlight"
        | "lava"
        | "ochre_froglight"
        | "verdant_froglight"
        | "pearlescent_froglight"
        | "beacon"
        | "end_rod"
        | "fire"
        | "soul_fire"
        | "jack_o_lantern"
        | "lantern"
        | "soul_lantern" => 1.0,
        "torch" | "wall_torch" | "soul_torch" | "soul_wall_torch" | "copper_torch" | "copper_wall_torch" => 0.9,
        "redstone_lamp" | "redstone_ore" | "deepslate_redstone_ore" if state.is("lit", "true") => 0.9,
        "campfire" | "soul_campfire" if lit => 0.7,
        "redstone_torch" | "redstone_wall_torch" if lit => 0.5,
        "magma_block" | "nether_portal" | "sea_pickle" | "conduit" | "respawn_anchor" | "glow_lichen" => 0.5,
        "crying_obsidian" | "amethyst_cluster" => 0.3,
        _ if name.ends_with("candle") && state.is("lit", "true") => 0.6,
        _ => 0.0,
    }
}

/// A box in sixteenths.
const fn bx(x0: u8, y0: u8, z0: u8, x1: u8, y1: u8, z1: u8) -> [u8; 6] {
    [x0, y0, z0, x1, y1, z1]
}

/// A box against the wall on the side opposite `facing`, `depth`
/// sixteenths thick, from `y0` to `y1`: ladders, wall signs, open
/// trapdoors.
fn against(facing: Dir, depth: u8, y0: u8, y1: u8) -> [u8; 6] {
    match facing {
        Dir::North => bx(0, y0, 16 - depth, 16, y1, 16),
        Dir::South => bx(0, y0, 0, 16, y1, depth),
        Dir::East => bx(0, y0, 0, depth, y1, 16),
        _ => bx(16 - depth, y0, 0, 16, y1, 16),
    }
}

fn shape(state: &BlockState, colors: &Colors) -> Shape {
    let name = state.name.as_str();
    let facing = Dir::horizontal(state.prop("facing"));
    let top = state.is("half", "top") || state.is("type", "top");
    let ends = |s: &str| name.ends_with(s);
    let side_prop = |k: &str| -> Option<bool> { state.prop(k).map(|v| !matches!(v, "false" | "none")) };
    let sides = || [side_prop("north"), side_prop("east"), side_prop("south"), side_prop("west")];
    if name == "water" || name == "lava" || name == "bubble_column" {
        return Shape::Liquid;
    }
    if ends("_slab") {
        return match state.prop("type") {
            Some("double") => Shape::Cube,
            _ if top => Shape::Boxes(vec![bx(0, 8, 0, 16, 16, 16)]),
            _ => Shape::Boxes(vec![bx(0, 0, 0, 16, 8, 16)]),
        };
    }
    if ends("_stairs") {
        // The full-height step is on the side the stairs face.
        let (y0, y1, s0, s1) = if top { (0, 8, 8, 16) } else { (8, 16, 0, 8) };
        let step = match facing {
            Dir::North => bx(0, y0, 0, 16, y1, 8),
            Dir::South => bx(0, y0, 8, 16, y1, 16),
            Dir::East => bx(8, y0, 0, 16, y1, 16),
            _ => bx(0, y0, 0, 8, y1, 16),
        };
        return Shape::Boxes(vec![bx(0, s0, 0, 16, s1, 16), step]);
    }
    if ends("_fence_gate") {
        let along_x = matches!(facing, Dir::North | Dir::South);
        let boxes = [bx(0, 5, 7, 2, 16, 9), bx(14, 5, 7, 16, 16, 9), bx(2, 6, 7, 14, 9, 9), bx(2, 12, 7, 14, 15, 9)];
        let swap = |b: [u8; 6]| if along_x { b } else { [b[2], b[1], b[0], b[5], b[4], b[3]] };
        return Shape::Boxes(boxes.into_iter().map(swap).collect());
    }
    if ends("_fence") {
        return Shape::Connected(Connect::Fence, sides());
    }
    let wall_like =
        ["_wall_sign", "_wall_torch", "_wall_banner", "_wall_head", "_wall_skull", "_wall_fan", "_wall_hanging_sign"];
    if ends("_wall") && !wall_like.iter().any(|s| ends(s)) {
        return Shape::Connected(Connect::Wall, sides());
    }
    if ends("_pane") || name == "iron_bars" {
        return Shape::Connected(Connect::Pane, sides());
    }
    if ends("_carpet") {
        return Shape::Boxes(vec![bx(0, 0, 0, 16, 1, 16)]);
    }
    if name == "snow" {
        let layers: u8 = state.prop("layers").and_then(|v| v.parse().ok()).unwrap_or(1).clamp(1, 8);
        return if layers == 8 { Shape::Cube } else { Shape::Boxes(vec![bx(0, 0, 0, 16, 2 * layers, 16)]) };
    }
    if ends("_pressure_plate") {
        return Shape::Boxes(vec![bx(1, 0, 1, 15, 1, 15)]);
    }
    if ends("rail")
        || matches!(name, "redstone_wire" | "lily_pad" | "pink_petals" | "leaf_litter" | "wildflowers" | "frogspawn")
    {
        return Shape::Boxes(vec![bx(0, 0, 0, 16, 1, 16)]);
    }
    if ends("_trapdoor") {
        return if state.is("open", "true") {
            Shape::Boxes(vec![against(facing, 3, 0, 16)])
        } else if top {
            Shape::Boxes(vec![bx(0, 13, 0, 16, 16, 16)])
        } else {
            Shape::Boxes(vec![bx(0, 0, 0, 16, 3, 16)])
        };
    }
    if ends("_door") {
        // A closed door stands on the side away from where it faces; an
        // open one turns a quarter about its hinge.
        let mut d = facing;
        if state.is("open", "true") {
            let right = state.is("hinge", "right");
            d = match (d, right) {
                (Dir::North, false) | (Dir::South, true) => Dir::West,
                (Dir::North, true) | (Dir::South, false) => Dir::East,
                (Dir::East, false) | (Dir::West, true) => Dir::North,
                _ => Dir::South,
            };
        }
        let opposite = match d {
            Dir::North => Dir::South,
            Dir::South => Dir::North,
            Dir::East => Dir::West,
            _ => Dir::East,
        };
        return Shape::Boxes(vec![against(opposite, 3, 0, 16)]);
    }
    if matches!(name, "torch" | "soul_torch" | "redstone_torch" | "copper_torch") {
        return Shape::Boxes(vec![bx(7, 0, 7, 9, 10, 9)]);
    }
    if ends("wall_torch") {
        let b = against(facing, 2, 3, 13);
        // Narrow it to a stick in the middle of the wall.
        let stick = match facing {
            Dir::North | Dir::South => [7, b[1], b[2], 9, b[4], b[5]],
            _ => [b[0], b[1], 7, b[3], b[4], 9],
        };
        return Shape::Boxes(vec![stick]);
    }
    if name == "ladder" {
        return Shape::Boxes(vec![against(facing, 1, 0, 16)]);
    }
    if matches!(name, "vine" | "glow_lichen" | "sculk_vein" | "resin_clump") {
        let mut boxes = Vec::new();
        let on = |k: &str| state.is(k, "true");
        if on("north") {
            boxes.push(bx(0, 0, 0, 16, 16, 1));
        }
        if on("south") {
            boxes.push(bx(0, 0, 15, 16, 16, 16));
        }
        if on("west") {
            boxes.push(bx(0, 0, 0, 1, 16, 16));
        }
        if on("east") {
            boxes.push(bx(15, 0, 0, 16, 16, 16));
        }
        if on("up") {
            boxes.push(bx(0, 15, 0, 16, 16, 16));
        }
        if on("down") || boxes.is_empty() {
            boxes.push(bx(0, 0, 0, 16, 1, 16));
        }
        return Shape::Boxes(boxes);
    }
    if ends("_wall_sign") || ends("_wall_banner") {
        return Shape::Boxes(vec![against(facing, 2, 4, 12)]);
    }
    if ends("_wall_hanging_sign") || ends("_hanging_sign") {
        return Shape::Boxes(vec![bx(1, 0, 7, 15, 10, 9)]);
    }
    if ends("_sign") || ends("_banner") {
        let rotation: u8 = state.prop("rotation").and_then(|v| v.parse().ok()).unwrap_or(0);
        let across_x = matches!(rotation % 8, 0 | 1 | 7);
        let board = if across_x { bx(0, 7, 7, 16, 16, 9) } else { bx(7, 7, 0, 9, 16, 16) };
        return Shape::Boxes(vec![board, bx(7, 0, 7, 9, 7, 9)]);
    }
    if ends("_wall_head") || ends("_wall_skull") {
        return Shape::Boxes(vec![match facing {
            Dir::North => bx(4, 4, 8, 12, 12, 16),
            Dir::South => bx(4, 4, 0, 12, 12, 8),
            Dir::East => bx(0, 4, 4, 8, 12, 12),
            _ => bx(8, 4, 4, 16, 12, 12),
        }]);
    }
    if ends("_head") || ends("_skull") {
        return Shape::Boxes(vec![bx(4, 0, 4, 12, 8, 12)]);
    }
    if matches!(name, "lantern" | "soul_lantern")
        || ends("_lantern") && name != "sea_lantern" && name != "jack_o_lantern"
    {
        return Shape::Boxes(vec![if state.is("hanging", "true") {
            bx(5, 1, 5, 11, 10, 11)
        } else {
            bx(5, 0, 5, 11, 9, 11)
        }]);
    }
    if matches!(name, "chain" | "iron_chain" | "end_rod" | "lightning_rod" | "bamboo") {
        let axis = state.prop("axis").or(state.prop("facing"));
        return Shape::Boxes(vec![match axis {
            Some("x" | "east" | "west") => bx(0, 7, 7, 16, 9, 9),
            Some("z" | "north" | "south") => bx(7, 7, 0, 9, 9, 16),
            _ if name == "bamboo" => bx(6, 0, 6, 10, 16, 10),
            _ => bx(7, 0, 7, 9, 16, 9),
        }]);
    }
    if ends("_bed") {
        return Shape::Boxes(vec![bx(0, 0, 0, 16, 9, 16)]);
    }
    if ends("candle_cake") || name == "cake" {
        return Shape::Boxes(vec![bx(1, 0, 1, 15, 8, 15)]);
    }
    if ends("candle") {
        return Shape::Boxes(vec![bx(7, 0, 7, 9, 6, 9)]);
    }
    let fixed: Option<[u8; 6]> = match name {
        "chest" | "trapped_chest" | "ender_chest" => Some(bx(1, 0, 1, 15, 14, 15)),
        "cactus" => Some(bx(1, 0, 1, 15, 16, 15)),
        "flower_pot" => Some(bx(5, 0, 5, 11, 6, 11)),
        "enchanting_table" => Some(bx(0, 0, 0, 16, 12, 16)),
        "daylight_detector" => Some(bx(0, 0, 0, 16, 6, 16)),
        "stonecutter" => Some(bx(0, 0, 0, 16, 9, 16)),
        "campfire" | "soul_campfire" => Some(bx(0, 0, 0, 16, 7, 16)),
        "repeater" | "comparator" => Some(bx(0, 0, 0, 16, 2, 16)),
        "farmland" | "dirt_path" | "grass_path" => Some(bx(0, 0, 0, 16, 15, 16)),
        "anvil" | "chipped_anvil" | "damaged_anvil" => Some(bx(2, 0, 2, 14, 16, 14)),
        "brewing_stand" => Some(bx(7, 0, 7, 9, 14, 9)),
        "bell" => Some(bx(4, 4, 4, 12, 13, 12)),
        "sea_pickle" => Some(bx(6, 0, 6, 10, 6, 10)),
        "turtle_egg" | "sniffer_egg" => Some(bx(3, 0, 3, 13, 9, 13)),
        "conduit" => Some(bx(5, 5, 5, 11, 11, 11)),
        "dragon_egg" => Some(bx(1, 0, 1, 15, 16, 15)),
        "lever" => Some(bx(5, 0, 4, 11, 3, 12)),
        "tripwire_hook" => Some(bx(7, 4, 7, 9, 12, 9)),
        "end_portal" => Some(bx(0, 0, 0, 16, 12, 16)),
        "lectern" => Some(bx(0, 0, 0, 16, 13, 16)),
        "grindstone" => Some(bx(2, 4, 2, 14, 16, 14)),
        "decorated_pot" => Some(bx(1, 0, 1, 15, 16, 15)),
        "heavy_core" => Some(bx(4, 0, 4, 12, 8, 12)),
        "nether_portal" => Some(if state.is("axis", "z") { bx(6, 0, 0, 10, 16, 16) } else { bx(0, 0, 6, 16, 16, 10) }),
        _ if name.starts_with("potted_") => Some(bx(5, 0, 5, 11, 6, 11)),
        _ => None,
    };
    if let Some(b) = fixed {
        return Shape::Boxes(vec![b]);
    }
    if colors.side.1 == Pattern::Plant {
        return Shape::Cross;
    }
    Shape::Cube
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_parse() {
        let s = BlockState::parse("minecraft:oak_stairs[facing=east,half=top,waterlogged=false]");
        assert_eq!(s.name, "oak_stairs");
        assert_eq!((s.prop("facing"), s.prop("half")), (Some("east"), Some("top")));
        assert_eq!(BlockState::parse("minecraft:stone"), BlockState::new("stone", &[]));
        assert_eq!(BlockState::parse("create:shaft[axis=y]").name, "create:shaft");
    }

    #[test]
    fn shapes() {
        let l = |s: &str| look(&BlockState::parse(s)).shape;
        assert_eq!(l("stone"), Shape::Cube);
        assert_eq!(l("air"), Shape::Air);
        assert_eq!(l("cave_air"), Shape::Air);
        assert_eq!(l("stone_slab[type=top]"), Shape::Boxes(vec![[0, 8, 0, 16, 16, 16]]));
        assert_eq!(l("stone_slab[type=double]"), Shape::Cube);
        assert_eq!(
            l("oak_stairs[facing=east,half=bottom]"),
            Shape::Boxes(vec![[0, 0, 0, 16, 8, 16], [8, 8, 0, 16, 16, 16]])
        );
        assert!(matches!(
            l("oak_fence[north=true,south=false,east=false,west=false]"),
            Shape::Connected(Connect::Fence, [Some(true), Some(false), Some(false), Some(false)])
        ));
        assert!(matches!(
            l("cobblestone_wall[north=low,east=none]"),
            Shape::Connected(Connect::Wall, [Some(true), Some(false), None, None])
        ));
        assert!(matches!(l("glass_pane"), Shape::Connected(Connect::Pane, [None, None, None, None])));
        assert_eq!(l("poppy"), Shape::Cross);
        assert_eq!(l("short_grass"), Shape::Cross);
        assert_eq!(l("white_carpet"), Shape::Boxes(vec![[0, 0, 0, 16, 1, 16]]));
        assert_eq!(l("snow[layers=3]"), Shape::Boxes(vec![[0, 0, 0, 16, 6, 16]]));
        assert_eq!(l("oak_trapdoor[open=true,facing=north]"), Shape::Boxes(vec![[0, 0, 13, 16, 16, 16]]));
        assert_eq!(l("ladder[facing=east]"), Shape::Boxes(vec![[0, 0, 0, 1, 16, 16]]));
        assert_eq!(l("oak_wall_sign[facing=north]").clone(), Shape::Boxes(vec![[0, 4, 14, 16, 12, 16]]));
        assert_eq!(l("water[level=0]"), Shape::Liquid);
        let glass = look(&BlockState::parse("glass"));
        assert_eq!(glass.layer, Layer::Translucent);
        assert!(glass.hidden_by(&glass) && !glass.hidden_by(&look(&BlockState::parse("red_stained_glass"))));
        let stone = look(&BlockState::parse("stone"));
        assert!(stone.solid() && glass.hidden_by(&stone) && !stone.hidden_by(&glass));
        assert!(look(&BlockState::parse("glowstone")).glow > 0.0);
        assert_eq!(look(&BlockState::parse("oak_sapling")).layer, Layer::Cutout);
    }
}
