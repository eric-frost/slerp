//! Block ids and data values from before Minecraft 1.13, as MCEdit and
//! Schematica `.schematic` files store them, turned into today's block
//! states.  Written from the classic id list (0 to 255) that the
//! community documented; data values pick variants (wood types, wool
//! colours, stone kinds) and the orientation of stairs, slabs, logs,
//! doors, torches and the like where they matter to the drawing.

use crate::blocks::BlockState;
use crate::colors::DYES;

const WOOD: [&str; 6] = ["oak", "spruce", "birch", "jungle", "acacia", "dark_oak"];

/// The block state for a classic block id and data value.  Unknown ids
/// (mods, or ids past 255) become `legacy_<id>`, drawn grey.
pub fn state(id: u16, data: u8) -> BlockState {
    let d = data & 15;
    let s = |name: &str| BlockState::new(name, &[]);
    let p = |name: &str, props: &[(&str, &str)]| BlockState::new(name, props);
    let dye = DYES[d as usize];
    let horizontal = |v: u8| ["south", "west", "north", "east"][(v & 3) as usize];
    // Stairs store 0 east, 1 west, 2 south, 3 north, and 4 for upside down.
    let stairs = |name: &str| {
        p(
            name,
            &[
                ("facing", ["east", "west", "south", "north"][(d & 3) as usize]),
                ("half", if d & 4 != 0 { "top" } else { "bottom" }),
            ],
        )
    };
    let slab = |name: &str| p(name, &[("type", if d & 8 != 0 { "top" } else { "bottom" })]);
    let log = |name: String| {
        let axis = match (d >> 2) & 3 {
            1 => "x",
            2 => "z",
            _ => "y",
        };
        p(&name, &[("axis", axis)])
    };
    let door = |name: &str| {
        if d & 8 != 0 {
            p(name, &[("half", "upper")])
        } else {
            p(name, &[("half", "lower"), ("facing", ["east", "south", "west", "north"][(d & 3) as usize])])
        }
    };
    let torch = |name: &str, wall: &str| match d {
        1 => p(wall, &[("facing", "east")]),
        2 => p(wall, &[("facing", "west")]),
        3 => p(wall, &[("facing", "south")]),
        4 => p(wall, &[("facing", "north")]),
        _ => s(name),
    };
    let trapdoor = |name: &str| {
        p(
            name,
            &[
                ("facing", ["north", "south", "west", "east"][(d & 3) as usize]),
                ("open", if d & 4 != 0 { "true" } else { "false" }),
                ("half", if d & 8 != 0 { "top" } else { "bottom" }),
            ],
        )
    };
    let wood = |i: u8| WOOD[(i as usize).min(5)];
    match id {
        0 => s("air"),
        1 => {
            s(["stone", "granite", "polished_granite", "diorite", "polished_diorite", "andesite", "polished_andesite"]
                [(d as usize).min(6)])
        }
        2 => s("grass_block"),
        3 => s(["dirt", "coarse_dirt", "podzol"][(d as usize).min(2)]),
        4 => s("cobblestone"),
        5 => s(&format!("{}_planks", wood(d))),
        6 => s(&format!("{}_sapling", wood(d & 7))),
        7 => s("bedrock"),
        8 | 9 => s("water"),
        10 | 11 => s("lava"),
        12 => s(if d == 1 { "red_sand" } else { "sand" }),
        13 => s("gravel"),
        14 => s("gold_ore"),
        15 => s("iron_ore"),
        16 => s("coal_ore"),
        17 => log(format!("{}_log", wood(d & 3))),
        18 => s(&format!("{}_leaves", wood(d & 3))),
        19 => s(if d == 1 { "wet_sponge" } else { "sponge" }),
        20 => s("glass"),
        21 => s("lapis_ore"),
        22 => s("lapis_block"),
        23 => s("dispenser"),
        24 => s(["sandstone", "chiseled_sandstone", "cut_sandstone"][(d as usize).min(2)]),
        25 => s("note_block"),
        26 => s("red_bed"),
        27 => s("powered_rail"),
        28 => s("detector_rail"),
        29 => s("sticky_piston"),
        30 => s("cobweb"),
        31 => s(["dead_bush", "short_grass", "fern"][(d as usize).min(2)]),
        32 => s("dead_bush"),
        33 => s("piston"),
        34 => s("piston_head"),
        35 => s(&format!("{dye}_wool")),
        36 => s("moving_piston"),
        37 => s("dandelion"),
        38 => s([
            "poppy",
            "blue_orchid",
            "allium",
            "azure_bluet",
            "red_tulip",
            "orange_tulip",
            "white_tulip",
            "pink_tulip",
            "oxeye_daisy",
        ][(d as usize).min(8)]),
        39 => s("brown_mushroom"),
        40 => s("red_mushroom"),
        41 => s("gold_block"),
        42 => s("iron_block"),
        43 => match d & 7 {
            1 => s("sandstone"),
            2 => s("oak_planks"),
            3 => s("cobblestone"),
            4 => s("bricks"),
            5 => s("stone_bricks"),
            6 => s("nether_bricks"),
            7 => s("quartz_block"),
            _ => s("smooth_stone"),
        },
        44 => slab(
            [
                "smooth_stone_slab",
                "sandstone_slab",
                "petrified_oak_slab",
                "cobblestone_slab",
                "brick_slab",
                "stone_brick_slab",
                "nether_brick_slab",
                "quartz_slab",
            ][(d & 7) as usize],
        ),
        45 => s("bricks"),
        46 => s("tnt"),
        47 => s("bookshelf"),
        48 => s("mossy_cobblestone"),
        49 => s("obsidian"),
        50 => torch("torch", "wall_torch"),
        51 => s("fire"),
        52 => s("spawner"),
        53 => stairs("oak_stairs"),
        54 => s("chest"),
        55 => s("redstone_wire"),
        56 => s("diamond_ore"),
        57 => s("diamond_block"),
        58 => s("crafting_table"),
        59 => s("wheat"),
        60 => s("farmland"),
        61 | 62 => p("furnace", &[("facing", horizontal(d))]),
        63 => p("oak_sign", &[("rotation", ["0", "4", "8", "12"][((d >> 2) & 3) as usize])]),
        64 => door("oak_door"),
        65 => p("ladder", &[("facing", ["north", "north", "north", "south", "west", "east"][(d as usize).min(5)])]),
        66 => s("rail"),
        67 => stairs("cobblestone_stairs"),
        68 => {
            p("oak_wall_sign", &[("facing", ["north", "north", "north", "south", "west", "east"][(d as usize).min(5)])])
        }
        69 => s("lever"),
        70 => s("stone_pressure_plate"),
        71 => door("iron_door"),
        72 => s("oak_pressure_plate"),
        73 => s("redstone_ore"),
        74 => p("redstone_ore", &[("lit", "true")]),
        75 | 76 => torch("redstone_torch", "redstone_wall_torch"),
        77 => s("stone_button"),
        78 => p("snow", &[("layers", ["1", "2", "3", "4", "5", "6", "7", "8"][(d & 7) as usize])]),
        79 => s("ice"),
        80 => s("snow_block"),
        81 => s("cactus"),
        82 => s("clay"),
        83 => s("sugar_cane"),
        84 => s("jukebox"),
        85 => s("oak_fence"),
        86 => s("carved_pumpkin"),
        87 => s("netherrack"),
        88 => s("soul_sand"),
        89 => s("glowstone"),
        90 => s("nether_portal"),
        91 => s("jack_o_lantern"),
        92 => s("cake"),
        93 | 94 => s("repeater"),
        95 => s(&format!("{dye}_stained_glass")),
        96 => trapdoor("oak_trapdoor"),
        97 => s(["infested_stone", "infested_cobblestone", "infested_stone_bricks"][(d as usize).min(2)]),
        98 => {
            s(["stone_bricks", "mossy_stone_bricks", "cracked_stone_bricks", "chiseled_stone_bricks"][(d & 3) as usize])
        }
        99 => s("brown_mushroom_block"),
        100 => s("red_mushroom_block"),
        101 => s("iron_bars"),
        102 => s("glass_pane"),
        103 => s("melon"),
        104 => s("pumpkin_stem"),
        105 => s("melon_stem"),
        106 => s("vine"),
        107 => p("oak_fence_gate", &[("facing", horizontal(d))]),
        108 => stairs("brick_stairs"),
        109 => stairs("stone_brick_stairs"),
        110 => s("mycelium"),
        111 => s("lily_pad"),
        112 => s("nether_bricks"),
        113 => s("nether_brick_fence"),
        114 => stairs("nether_brick_stairs"),
        115 => s("nether_wart"),
        116 => s("enchanting_table"),
        117 => s("brewing_stand"),
        118 => s("cauldron"),
        119 => s("end_portal"),
        120 => s("end_portal_frame"),
        121 => s("end_stone"),
        122 => s("dragon_egg"),
        123 => s("redstone_lamp"),
        124 => p("redstone_lamp", &[("lit", "true")]),
        125 => s(&format!("{}_planks", wood(d & 7))),
        126 => slab(&format!("{}_slab", wood(d & 7))),
        127 => s("cocoa"),
        128 => stairs("sandstone_stairs"),
        129 => s("emerald_ore"),
        130 => s("ender_chest"),
        131 => s("tripwire_hook"),
        132 => s("tripwire"),
        133 => s("emerald_block"),
        134 => stairs("spruce_stairs"),
        135 => stairs("birch_stairs"),
        136 => stairs("jungle_stairs"),
        137 => s("command_block"),
        138 => s("beacon"),
        139 => s(if d == 1 { "mossy_cobblestone_wall" } else { "cobblestone_wall" }),
        140 => s("flower_pot"),
        141 => s("carrots"),
        142 => s("potatoes"),
        143 => s("oak_button"),
        144 => s("skeleton_skull"),
        145 => s("anvil"),
        146 => s("trapped_chest"),
        147 => s("light_weighted_pressure_plate"),
        148 => s("heavy_weighted_pressure_plate"),
        149 | 150 => s("comparator"),
        151 | 178 => s("daylight_detector"),
        152 => s("redstone_block"),
        153 => s("nether_quartz_ore"),
        154 => s("hopper"),
        155 => match d {
            1 => s("chiseled_quartz_block"),
            2..=4 => p("quartz_pillar", &[("axis", ["y", "x", "z"][(d - 2) as usize])]),
            _ => s("quartz_block"),
        },
        156 => stairs("quartz_stairs"),
        157 => s("activator_rail"),
        158 => s("dropper"),
        159 => s(&format!("{dye}_terracotta")),
        160 => s(&format!("{dye}_stained_glass_pane")),
        161 => s(&format!("{}_leaves", wood(4 + (d & 1)))),
        162 => log(format!("{}_log", wood(4 + (d & 1)))),
        163 => stairs("acacia_stairs"),
        164 => stairs("dark_oak_stairs"),
        165 => s("slime_block"),
        166 => s("barrier"),
        167 => trapdoor("iron_trapdoor"),
        168 => s(["prismarine", "prismarine_bricks", "dark_prismarine"][(d as usize).min(2)]),
        169 => s("sea_lantern"),
        170 => s("hay_block"),
        171 => s(&format!("{dye}_carpet")),
        172 => s("terracotta"),
        173 => s("coal_block"),
        174 => s("packed_ice"),
        175 => match d & 7 {
            1 => s("lilac"),
            2 => s("tall_grass"),
            3 => s("large_fern"),
            4 => s("rose_bush"),
            5 => s("peony"),
            _ if d & 8 != 0 => s("tall_grass"),
            _ => s("sunflower"),
        },
        176 => s("white_banner"),
        177 => s("white_wall_banner"),
        179 => s(["red_sandstone", "chiseled_red_sandstone", "cut_red_sandstone"][(d as usize).min(2)]),
        180 => stairs("red_sandstone_stairs"),
        181 => s("red_sandstone"),
        182 => slab("red_sandstone_slab"),
        183..=187 => p(
            &format!("{}_fence_gate", ["spruce", "birch", "jungle", "dark_oak", "acacia"][(id - 183) as usize]),
            &[("facing", horizontal(d))],
        ),
        188..=192 => s(&format!("{}_fence", ["spruce", "birch", "jungle", "dark_oak", "acacia"][(id - 188) as usize])),
        193..=197 => {
            door(&format!("{}_door", ["spruce", "birch", "jungle", "acacia", "dark_oak"][(id - 193) as usize]))
        }
        198 => s("end_rod"),
        199 => s("chorus_plant"),
        200 => s("chorus_flower"),
        201 => s("purpur_block"),
        202 => s("purpur_pillar"),
        203 => stairs("purpur_stairs"),
        204 => s("purpur_block"),
        205 => slab("purpur_slab"),
        206 => s("end_stone_bricks"),
        207 => s("beetroots"),
        208 => s("dirt_path"),
        209 => s("end_gateway"),
        210 => s("repeating_command_block"),
        211 => s("chain_command_block"),
        212 => s("frosted_ice"),
        213 => s("magma_block"),
        214 => s("nether_wart_block"),
        215 => s("red_nether_bricks"),
        216 => s("bone_block"),
        217 => s("structure_void"),
        218 => s("observer"),
        219..=234 => s(&format!("{}_shulker_box", DYES[(id - 219) as usize])),
        235..=250 => s(&format!("{}_glazed_terracotta", DYES[(id - 235) as usize])),
        251 => s(&format!("{dye}_concrete")),
        252 => s(&format!("{dye}_concrete_powder")),
        255 => s("structure_block"),
        _ => s(&format!("legacy_{id}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classic_ids() {
        assert_eq!(state(1, 0).name, "stone");
        assert_eq!(state(1, 5).name, "andesite");
        assert_eq!(state(5, 5).name, "dark_oak_planks");
        assert_eq!(state(35, 14).name, "red_wool");
        assert_eq!(state(35, 3).name, "light_blue_wool");
        assert_eq!(state(159, 8).name, "light_gray_terracotta");
        assert_eq!(state(162, 1).name, "dark_oak_log");
        assert_eq!(state(17, 2 | 4).props, [("axis".to_string(), "x".to_string())]);
        let st = state(53, 2 | 4);
        assert_eq!((st.name.as_str(), st.prop("facing"), st.prop("half")), ("oak_stairs", Some("south"), Some("top")));
        assert_eq!(state(44, 8 | 5).prop("type"), Some("top"));
        assert_eq!(state(44, 5).name, "stone_brick_slab");
        assert_eq!(state(50, 3).name, "wall_torch");
        assert_eq!(state(64, 8).prop("half"), Some("upper"));
        assert_eq!(state(78, 2).prop("layers"), Some("3"));
        assert_eq!(state(251, 15).name, "black_concrete");
        assert_eq!(state(235, 0).name, "white_glazed_terracotta");
        assert_eq!(state(1000, 0).name, "legacy_1000");
        // Every classic id names a block the colour table knows, apart from
        // the invisible ones.
        for id in 0..=255u16 {
            for d in 0..16u8 {
                let st = state(id, d);
                if st.name.starts_with("legacy_") || crate::blocks::invisible(&st.name) {
                    continue;
                }
                assert!(crate::colors::lookup(&st.name).1, "{id}:{d} {}", st.name);
            }
        }
    }
}
