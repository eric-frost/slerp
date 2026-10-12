//! Block colours by name, written for Slerp: each is the average colour a
//! block's faces show in the game (grass and leaves as a temperate biome
//! tints them), judged by eye, with no game files read or shipped.  Faces
//! that differ (grass blocks, logs, pumpkins) have their own colours.
//!
//! Families (wood types, dye colours, stone variants and the stairs, slabs,
//! walls and fences made from them) are worked out from the name, so the
//! table covers about a thousand block names with a few hundred entries.
//! Names nothing matches are grey and counted, so the viewer can say how
//! many kinds it guessed.

/// A procedural texture the faces wear, multiplied with their colour, so
/// large flat areas read as blocks rather than paint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Pattern {
    /// Fine noise: stone, dirt, sand, concrete powder.
    Noise,
    /// Barely any variation: concrete, metal blocks, quartz.
    Smooth,
    /// Horizontal boards.
    Planks,
    /// Courses of bricks with mortar.
    Bricks,
    /// Irregular stones: cobblestone.
    Cobble,
    /// Vertical streaks: the bark of logs.
    Bark,
    /// Growth rings: the ends of logs.
    Rings,
    /// Dense speckles: leaves, moss, wool.
    Leaves,
    /// A frame around a clear centre: glass.
    Glass,
    /// Blades and petals with clear gaps: plants, drawn crossed.
    Plant,
    /// Stone with bright flecks: ores.
    Ore,
    /// Square tiles.
    Tiles,
}

/// The colours of one block: its top, sides and bottom.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Colors {
    pub top: ([u8; 3], Pattern),
    pub side: ([u8; 3], Pattern),
    pub bottom: ([u8; 3], Pattern),
}

impl Colors {
    const fn all(c: [u8; 3], p: Pattern) -> Colors {
        Colors { top: (c, p), side: (c, p), bottom: (c, p) }
    }
}

use Pattern::*;

/// A block with its top, side and bottom colours and its sides' pattern.
type Faced = (&'static str, [u8; 3], [u8; 3], [u8; 3], Pattern);

/// Blocks whose top and bottom differ from their sides.
const FACED: &[Faced] = &[
    // name, top, side, bottom, side pattern
    ("grass_block", [96, 152, 58], [125, 108, 72], [134, 96, 67], Noise),
    ("podzol", [92, 64, 26], [122, 88, 58], [134, 96, 67], Noise),
    ("mycelium", [112, 98, 104], [118, 97, 84], [134, 96, 67], Noise),
    ("dirt_path", [148, 122, 65], [140, 104, 68], [134, 96, 67], Noise),
    ("farmland", [100, 62, 34], [134, 96, 67], [134, 96, 67], Noise),
    ("crimson_nylium", [131, 30, 30], [107, 47, 44], [97, 38, 38], Noise),
    ("warped_nylium", [43, 114, 101], [80, 64, 58], [97, 38, 38], Noise),
    ("sandstone", [223, 214, 170], [216, 203, 155], [221, 208, 158], Noise),
    ("red_sandstone", [181, 98, 31], [186, 99, 29], [184, 98, 30], Noise),
    ("chiseled_sandstone", [223, 214, 170], [216, 202, 155], [223, 214, 170], Tiles),
    ("cut_sandstone", [223, 214, 170], [217, 206, 159], [223, 214, 170], Smooth),
    ("chiseled_red_sandstone", [181, 98, 31], [183, 96, 27], [181, 98, 31], Tiles),
    ("cut_red_sandstone", [181, 98, 31], [189, 101, 32], [181, 98, 31], Smooth),
    ("pumpkin", [198, 118, 24], [198, 118, 24], [198, 118, 24], Noise),
    ("carved_pumpkin", [198, 118, 24], [150, 84, 17], [198, 118, 24], Noise),
    ("jack_o_lantern", [198, 118, 24], [215, 152, 53], [198, 118, 24], Noise),
    ("melon", [111, 145, 30], [111, 145, 31], [111, 145, 30], Noise),
    ("hay_block", [166, 136, 38], [166, 139, 12], [166, 136, 38], Bark),
    ("bone_block", [210, 206, 179], [229, 225, 208], [210, 206, 179], Bark),
    ("quartz_pillar", [235, 230, 224], [236, 231, 224], [235, 230, 224], Bark),
    ("purpur_pillar", [172, 130, 172], [171, 129, 171], [172, 130, 172], Bark),
    ("basalt", [81, 81, 86], [73, 72, 78], [81, 81, 86], Bark),
    ("polished_basalt", [99, 98, 100], [88, 88, 91], [99, 98, 100], Bark),
    ("crafting_table", [120, 73, 42], [119, 92, 59], [162, 130, 78], Planks),
    ("bookshelf", [162, 130, 78], [117, 94, 59], [162, 130, 78], Planks),
    ("chiseled_bookshelf", [175, 141, 86], [94, 77, 50], [175, 141, 86], Planks),
    ("tnt", [143, 62, 54], [182, 88, 84], [168, 78, 67], Smooth),
    ("furnace", [110, 110, 110], [104, 104, 104], [110, 110, 110], Cobble),
    ("blast_furnace", [80, 80, 81], [108, 107, 107], [80, 80, 81], Cobble),
    ("smoker", [85, 83, 81], [107, 88, 64], [118, 117, 117], Cobble),
    ("barrel", [134, 100, 58], [108, 81, 50], [134, 100, 58], Planks),
    ("dispenser", [110, 110, 110], [100, 100, 100], [110, 110, 110], Cobble),
    ("dropper", [110, 110, 110], [100, 100, 100], [110, 110, 110], Cobble),
    ("observer", [98, 98, 98], [70, 69, 69], [98, 98, 98], Smooth),
    ("piston", [153, 127, 85], [110, 104, 96], [97, 97, 97], Smooth),
    ("sticky_piston", [110, 140, 90], [110, 104, 96], [97, 97, 97], Smooth),
    ("note_block", [88, 58, 40], [88, 58, 40], [88, 58, 40], Planks),
    ("jukebox", [94, 64, 47], [93, 64, 47], [93, 64, 47], Planks),
    ("cartography_table", [103, 87, 67], [72, 50, 34], [72, 50, 34], Planks),
    ("fletching_table", [197, 180, 133], [161, 140, 102], [197, 180, 133], Planks),
    ("smithing_table", [57, 58, 70], [64, 44, 40], [57, 58, 70], Smooth),
    ("loom", [142, 119, 92], [148, 124, 99], [142, 119, 92], Planks),
    ("beehive", [180, 146, 90], [158, 126, 72], [180, 146, 90], Planks),
    ("bee_nest", [202, 160, 74], [196, 150, 72], [202, 160, 74], Noise),
    ("target", [226, 170, 157], [226, 170, 157], [226, 170, 157], Rings),
    ("dried_kelp_block", [50, 58, 38], [38, 49, 30], [50, 58, 38], Noise),
    ("cactus", [85, 127, 43], [86, 127, 42], [85, 127, 43], Noise),
    ("redstone_lamp", [95, 54, 30], [95, 54, 30], [95, 54, 30], Tiles),
    ("deepslate", [80, 80, 82], [80, 80, 82], [80, 80, 82], Noise),
    ("respawn_anchor", [75, 26, 145], [33, 22, 51], [33, 22, 51], Smooth),
    ("lodestone", [147, 149, 152], [119, 120, 123], [147, 149, 152], Smooth),
    ("sculk_catalyst", [117, 125, 120], [72, 77, 78], [42, 47, 50], Noise),
    ("enchanting_table", [123, 37, 39], [43, 33, 43], [17, 13, 27], Smooth),
    ("command_block", [181, 136, 108], [181, 136, 108], [181, 136, 108], Tiles),
    ("chain_command_block", [131, 161, 147], [131, 161, 147], [131, 161, 147], Tiles),
    ("repeating_command_block", [129, 111, 176], [129, 111, 176], [129, 111, 176], Tiles),
    ("structure_block", [88, 74, 90], [88, 74, 90], [88, 74, 90], Tiles),
    ("jigsaw", [80, 70, 81], [80, 70, 81], [80, 70, 81], Tiles),
];

/// Single-colour blocks, sorted by name for binary search.
const PLAIN: &[(&str, [u8; 3], Pattern)] = &[
    ("amethyst_block", [133, 97, 191], Noise),
    ("amethyst_cluster", [164, 126, 207], Plant),
    ("ancient_debris", [94, 66, 58], Noise),
    ("andesite", [136, 136, 136], Noise),
    ("anvil", [68, 68, 68], Smooth),
    ("azalea", [101, 124, 47], Leaves),
    ("azalea_leaves", [90, 115, 45], Leaves),
    ("bamboo_block", [127, 144, 58], Bark),
    ("bamboo_mosaic", [190, 170, 78], Planks),
    ("beacon", [117, 220, 215], Glass),
    ("bedrock", [85, 85, 85], Cobble),
    ("bell", [200, 165, 60], Smooth),
    ("black_glazed_terracotta", [67, 30, 32], Tiles),
    ("blackstone", [42, 36, 41], Noise),
    ("blue_glazed_terracotta", [47, 64, 139], Tiles),
    ("blue_ice", [116, 167, 253], Smooth),
    ("brewing_stand", [122, 101, 82], Smooth),
    ("bricks", [150, 97, 83], Bricks),
    ("brown_glazed_terracotta", [119, 106, 85], Tiles),
    ("brown_mushroom_block", [149, 111, 81], Noise),
    ("budding_amethyst", [132, 96, 186], Noise),
    ("cake", [235, 220, 210], Smooth),
    ("calcite", [223, 224, 220], Noise),
    ("campfire", [110, 82, 50], Bark),
    ("candle", [225, 200, 160], Smooth),
    ("cauldron", [62, 62, 62], Smooth),
    ("chest", [162, 125, 60], Planks),
    ("chipped_anvil", [68, 68, 68], Smooth),
    ("chiseled_copper", [184, 101, 74], Tiles),
    ("chiseled_deepslate", [54, 54, 54], Tiles),
    ("chiseled_nether_bricks", [47, 23, 28], Bricks),
    ("chiseled_polished_blackstone", [53, 48, 56], Tiles),
    ("chiseled_quartz_block", [231, 226, 218], Tiles),
    ("chiseled_resin_bricks", [200, 98, 37], Tiles),
    ("chiseled_stone_bricks", [119, 118, 119], Tiles),
    ("chiseled_tuff", [89, 94, 87], Tiles),
    ("chiseled_tuff_bricks", [98, 102, 96], Tiles),
    ("clay", [160, 166, 179], Noise),
    ("closed_eyeblossom", [110, 100, 110], Plant),
    ("coal_block", [16, 15, 15], Noise),
    ("coal_ore", [105, 105, 105], Ore),
    ("coarse_dirt", [119, 85, 59], Noise),
    ("cobbled_deepslate", [77, 77, 80], Cobble),
    ("cobblestone", [127, 127, 127], Cobble),
    ("cobweb", [228, 233, 234], Plant),
    ("comparator", [166, 161, 159], Smooth),
    ("composter", [88, 61, 23], Planks),
    ("conduit", [159, 139, 113], Smooth),
    ("copper_block", [192, 107, 79], Smooth),
    ("copper_ore", [124, 125, 120], Ore),
    ("cracked_deepslate_bricks", [64, 64, 65], Bricks),
    ("cracked_deepslate_tiles", [52, 52, 52], Tiles),
    ("cracked_nether_bricks", [40, 20, 23], Bricks),
    ("cracked_polished_blackstone_bricks", [44, 37, 43], Bricks),
    ("cracked_stone_bricks", [118, 117, 118], Bricks),
    ("crafter", [110, 100, 100], Tiles),
    ("creaking_heart", [80, 70, 60], Bark),
    ("crimson_fungus", [141, 44, 29], Plant),
    ("crimson_roots", [126, 8, 41], Plant),
    ("crying_obsidian", [32, 10, 60], Noise),
    ("cut_copper", [191, 106, 80], Tiles),
    ("cyan_glazed_terracotta", [52, 118, 125], Tiles),
    ("damaged_anvil", [68, 68, 68], Smooth),
    ("dark_prismarine", [51, 91, 75], Tiles),
    ("daylight_detector", [130, 116, 94], Smooth),
    ("dead_bush", [107, 78, 40], Plant),
    ("decorated_pot", [140, 80, 60], Smooth),
    ("deepslate_bricks", [70, 70, 71], Bricks),
    ("deepslate_tiles", [54, 54, 55], Tiles),
    ("diamond_block", [98, 237, 228], Smooth),
    ("diamond_ore", [121, 141, 140], Ore),
    ("diorite", [188, 188, 188], Noise),
    ("dirt", [134, 96, 67], Noise),
    ("dragon_egg", [12, 9, 15], Smooth),
    ("dragon_head", [30, 30, 32], Smooth),
    ("dragon_wall_head", [30, 30, 32], Smooth),
    ("dried_ghast", [180, 170, 160], Smooth),
    ("dripstone_block", [134, 107, 92], Noise),
    ("emerald_block", [42, 203, 87], Smooth),
    ("emerald_ore", [117, 136, 124], Ore),
    ("end_portal_frame", [91, 120, 97], Tiles),
    ("end_rod", [220, 210, 200], Smooth),
    ("end_stone", [219, 222, 158], Noise),
    ("end_stone_bricks", [218, 224, 162], Bricks),
    ("exposed_chiseled_copper", [154, 119, 100], Tiles),
    ("exposed_copper", [161, 125, 103], Smooth),
    ("exposed_cut_copper", [154, 121, 101], Tiles),
    ("fire", [222, 140, 40], Plant),
    ("flowering_azalea", [112, 121, 74], Leaves),
    ("flowering_azalea_leaves", [100, 112, 70], Leaves),
    ("frogspawn", [107, 94, 85], Plant),
    ("gilded_blackstone", [56, 43, 38], Ore),
    ("glass", [175, 213, 219], Glass),
    ("glass_pane", [175, 213, 219], Glass),
    ("glowstone", [171, 131, 84], Noise),
    ("gold_block", [246, 208, 61], Smooth),
    ("gold_ore", [145, 133, 106], Ore),
    ("granite", [149, 103, 85], Noise),
    ("grass_path", [148, 122, 65], Noise),
    ("gravel", [131, 127, 126], Cobble),
    ("gray_glazed_terracotta", [83, 90, 93], Tiles),
    ("green_glazed_terracotta", [117, 142, 67], Tiles),
    ("grindstone", [142, 142, 142], Smooth),
    ("hanging_roots", [161, 115, 91], Plant),
    ("heavy_core", [80, 82, 90], Smooth),
    ("honey_block", [251, 185, 52], Smooth),
    ("honeycomb_block", [229, 148, 29], Tiles),
    ("hopper", [68, 68, 70], Smooth),
    ("ice", [145, 183, 253], Smooth),
    ("iron_bars", [136, 139, 135], Smooth),
    ("iron_block", [220, 220, 220], Smooth),
    ("iron_ore", [136, 129, 122], Ore),
    ("ladder", [124, 98, 58], Planks),
    ("lantern", [106, 91, 83], Smooth),
    ("lapis_block", [30, 67, 140], Noise),
    ("lapis_ore", [99, 110, 132], Ore),
    ("large_amethyst_bud", [164, 126, 207], Plant),
    ("lava", [207, 92, 15], Noise),
    ("lava_cauldron", [62, 62, 62], Smooth),
    ("lectern", [173, 137, 83], Planks),
    ("light_blue_glazed_terracotta", [94, 164, 208], Tiles),
    ("light_gray_glazed_terracotta", [144, 166, 167], Tiles),
    ("lightning_rod", [194, 110, 83], Smooth),
    ("lily_pad", [32, 128, 48], Plant),
    ("lime_glazed_terracotta", [162, 197, 55], Tiles),
    ("magenta_glazed_terracotta", [208, 100, 191], Tiles),
    ("magma_block", [142, 63, 31], Noise),
    ("mangrove_propagule", [90, 130, 50], Plant),
    ("mangrove_roots", [74, 59, 38], Bark),
    ("medium_amethyst_bud", [164, 126, 207], Plant),
    ("moss_block", [89, 109, 45], Leaves),
    ("moss_carpet", [89, 109, 45], Leaves),
    ("mossy_cobblestone", [110, 118, 94], Cobble),
    ("mossy_stone_bricks", [115, 121, 105], Bricks),
    ("mud", [60, 57, 60], Noise),
    ("mud_bricks", [137, 103, 79], Bricks),
    ("muddy_mangrove_roots", [70, 59, 45], Noise),
    ("mushroom_stem", [203, 196, 185], Noise),
    ("nether_bricks", [44, 21, 26], Bricks),
    ("nether_gold_ore", [115, 54, 42], Ore),
    ("nether_portal", [89, 11, 192], Smooth),
    ("nether_quartz_ore", [117, 65, 62], Ore),
    ("nether_sprouts", [20, 151, 133], Plant),
    ("nether_wart", [111, 18, 19], Plant),
    ("nether_wart_block", [115, 3, 3], Noise),
    ("netherite_block", [66, 61, 63], Smooth),
    ("netherrack", [97, 38, 38], Noise),
    ("obsidian", [15, 10, 24], Noise),
    ("ochre_froglight", [250, 245, 206], Smooth),
    ("open_eyeblossom", [230, 140, 50], Plant),
    ("orange_glazed_terracotta", [154, 147, 91], Tiles),
    ("oxidized_chiseled_copper", [84, 155, 128], Tiles),
    ("oxidized_copper", [82, 162, 132], Smooth),
    ("oxidized_cut_copper", [79, 153, 126], Tiles),
    ("packed_ice", [141, 180, 250], Smooth),
    ("packed_mud", [142, 106, 79], Noise),
    ("pale_hanging_moss", [107, 112, 105], Plant),
    ("pale_moss_block", [107, 112, 105], Leaves),
    ("pale_moss_carpet", [107, 112, 105], Leaves),
    ("pearlescent_froglight", [245, 240, 239], Smooth),
    ("piglin_head", [200, 140, 120], Smooth),
    ("piglin_wall_head", [200, 140, 120], Smooth),
    ("pink_glazed_terracotta", [235, 154, 181], Tiles),
    ("pink_petals", [235, 160, 190], Plant),
    ("piston_head", [153, 127, 85], Planks),
    ("pointed_dripstone", [129, 102, 89], Plant),
    ("polished_andesite", [132, 134, 133], Smooth),
    ("polished_blackstone", [53, 48, 56], Smooth),
    ("polished_blackstone_bricks", [48, 42, 49], Bricks),
    ("polished_deepslate", [72, 72, 73], Smooth),
    ("polished_diorite", [192, 193, 194], Smooth),
    ("polished_granite", [154, 106, 89], Smooth),
    ("polished_tuff", [97, 104, 99], Smooth),
    ("powder_snow", [248, 253, 253], Smooth),
    ("powder_snow_cauldron", [62, 62, 62], Smooth),
    ("prismarine", [99, 156, 151], Noise),
    ("prismarine_bricks", [99, 171, 158], Bricks),
    ("purple_glazed_terracotta", [109, 48, 152], Tiles),
    ("purpur_block", [169, 125, 169], Tiles),
    ("quartz_block", [235, 229, 222], Smooth),
    ("quartz_bricks", [234, 229, 221], Bricks),
    ("raw_copper_block", [154, 105, 79], Noise),
    ("raw_gold_block", [221, 169, 46], Noise),
    ("raw_iron_block", [166, 135, 107], Noise),
    ("red_glazed_terracotta", [181, 59, 53], Tiles),
    ("red_mushroom_block", [200, 46, 45], Noise),
    ("red_nether_bricks", [69, 7, 9], Bricks),
    ("red_sand", [190, 102, 33], Noise),
    ("redstone_block", [175, 24, 5], Noise),
    ("redstone_ore", [133, 107, 107], Ore),
    ("redstone_wire", [175, 24, 5], Smooth),
    ("reinforced_deepslate", [80, 82, 78], Tiles),
    ("repeater", [160, 156, 152], Smooth),
    ("resin_block", [217, 99, 25], Noise),
    ("resin_bricks", [206, 101, 39], Bricks),
    ("resin_clump", [217, 99, 25], Plant),
    ("rooted_dirt", [144, 103, 76], Noise),
    ("sand", [219, 207, 163], Noise),
    ("scaffolding", [170, 132, 73], Planks),
    ("sculk", [12, 29, 36], Noise),
    ("sculk_sensor", [7, 70, 84], Noise),
    ("sculk_shrieker", [198, 205, 169], Noise),
    ("sea_lantern", [172, 199, 190], Tiles),
    ("sea_pickle", [90, 97, 39], Smooth),
    ("seagrass", [42, 112, 30], Plant),
    ("shroomlight", [240, 146, 70], Noise),
    ("shulker_box", [151, 103, 151], Smooth),
    ("slime_block", [111, 192, 91], Smooth),
    ("small_amethyst_bud", [164, 126, 207], Plant),
    ("smooth_basalt", [72, 72, 78], Smooth),
    ("smooth_quartz", [236, 230, 223], Smooth),
    ("smooth_red_sandstone", [181, 98, 31], Smooth),
    ("smooth_sandstone", [223, 214, 170], Smooth),
    ("smooth_stone", [158, 158, 158], Smooth),
    ("sniffer_egg", [140, 70, 60], Smooth),
    ("snow", [249, 254, 254], Smooth),
    ("snow_block", [249, 254, 254], Smooth),
    ("soul_campfire", [110, 82, 50], Bark),
    ("soul_fire", [52, 200, 210], Plant),
    ("soul_lantern", [71, 99, 114], Smooth),
    ("soul_sand", [81, 62, 51], Noise),
    ("soul_soil", [75, 57, 46], Noise),
    ("soul_torch", [110, 85, 50], Smooth),
    ("spawner", [36, 46, 62], Glass),
    ("sponge", [195, 192, 74], Noise),
    ("spore_blossom", [207, 97, 159], Plant),
    ("stone", [125, 125, 125], Noise),
    ("stone_bricks", [122, 121, 122], Bricks),
    ("stonecutter", [123, 118, 111], Smooth),
    ("sugar_cane", [148, 192, 101], Plant),
    ("suspicious_gravel", [129, 124, 122], Cobble),
    ("suspicious_sand", [217, 204, 159], Noise),
    ("sweet_berry_bush", [67, 98, 50], Plant),
    ("terracotta", [152, 94, 68], Noise),
    ("tinted_glass", [44, 38, 46], Glass),
    ("trapped_chest", [162, 125, 60], Planks),
    ("trial_spawner", [60, 70, 80], Glass),
    ("tripwire_hook", [140, 140, 140], Smooth),
    ("tuff", [108, 109, 102], Noise),
    ("tuff_bricks", [98, 102, 96], Bricks),
    ("turtle_egg", [228, 226, 191], Smooth),
    ("twisting_vines", [20, 143, 124], Plant),
    ("vault", [60, 70, 80], Tiles),
    ("verdant_froglight", [229, 244, 228], Smooth),
    ("vine", [61, 104, 31], Plant),
    ("warped_fungus", [74, 109, 87], Plant),
    ("warped_roots", [20, 138, 124], Plant),
    ("warped_wart_block", [22, 119, 121], Noise),
    ("water", [52, 92, 200], Smooth),
    ("water_cauldron", [62, 62, 62], Smooth),
    ("weathered_chiseled_copper", [104, 150, 106], Tiles),
    ("weathered_copper", [108, 153, 110], Smooth),
    ("weathered_cut_copper", [109, 145, 107], Tiles),
    ("weeping_vines", [105, 1, 0], Plant),
    ("wet_sponge", [171, 181, 70], Noise),
    ("white_glazed_terracotta", [188, 212, 203], Tiles),
    ("wither_rose", [41, 44, 23], Plant),
    ("yellow_glazed_terracotta", [234, 192, 89], Tiles),
];

/// The sixteen dye colours, in Minecraft's order.
pub const DYES: [&str; 16] = [
    "white",
    "orange",
    "magenta",
    "light_blue",
    "yellow",
    "lime",
    "pink",
    "gray",
    "light_gray",
    "cyan",
    "purple",
    "blue",
    "brown",
    "green",
    "red",
    "black",
];

const WOOL: [[u8; 3]; 16] = [
    [234, 236, 237],
    [241, 118, 20],
    [190, 69, 180],
    [58, 175, 217],
    [249, 198, 40],
    [112, 185, 26],
    [238, 141, 172],
    [63, 68, 72],
    [142, 142, 135],
    [21, 138, 145],
    [122, 42, 173],
    [53, 57, 157],
    [114, 72, 41],
    [85, 110, 28],
    [161, 39, 35],
    [21, 21, 26],
];

const CONCRETE: [[u8; 3]; 16] = [
    [207, 213, 214],
    [224, 97, 1],
    [169, 48, 159],
    [36, 137, 199],
    [241, 175, 21],
    [94, 169, 25],
    [214, 101, 143],
    [55, 58, 62],
    [125, 125, 115],
    [21, 119, 136],
    [100, 32, 156],
    [45, 47, 143],
    [96, 60, 32],
    [73, 91, 36],
    [142, 33, 33],
    [8, 10, 15],
];

const POWDER: [[u8; 3]; 16] = [
    [226, 227, 228],
    [227, 132, 32],
    [193, 84, 185],
    [74, 181, 213],
    [233, 199, 55],
    [125, 189, 42],
    [229, 153, 181],
    [77, 81, 85],
    [155, 155, 148],
    [37, 148, 157],
    [132, 56, 178],
    [70, 73, 167],
    [126, 85, 54],
    [97, 119, 45],
    [168, 54, 51],
    [25, 27, 32],
];

const TERRACOTTA: [[u8; 3]; 16] = [
    [210, 178, 161],
    [162, 84, 38],
    [150, 88, 109],
    [113, 109, 138],
    [186, 133, 35],
    [104, 118, 53],
    [162, 78, 79],
    [58, 42, 36],
    [135, 107, 98],
    [87, 91, 91],
    [118, 70, 86],
    [74, 60, 91],
    [77, 51, 36],
    [76, 83, 42],
    [143, 61, 47],
    [37, 23, 17],
];

/// Stained glass, which is mostly see-through: the dye's own colour.
const GLASS: [[u8; 3]; 16] = [
    [255, 255, 255],
    [216, 127, 51],
    [178, 76, 216],
    [102, 153, 216],
    [229, 229, 51],
    [127, 204, 25],
    [242, 127, 165],
    [76, 76, 76],
    [153, 153, 153],
    [76, 127, 153],
    [127, 63, 178],
    [51, 76, 178],
    [102, 76, 51],
    [102, 127, 51],
    [153, 51, 51],
    [25, 25, 25],
];

/// A wood type: name, planks, bark, log ends, leaves.
type Wood = (&'static str, [u8; 3], [u8; 3], [u8; 3], [u8; 3]);

const WOODS: &[Wood] = &[
    ("oak", [162, 130, 78], [109, 85, 50], [151, 121, 73], [59, 118, 30]),
    ("spruce", [114, 84, 48], [58, 37, 16], [108, 80, 46], [52, 89, 52]),
    ("birch", [192, 175, 121], [216, 215, 210], [193, 179, 135], [94, 132, 55]),
    ("jungle", [160, 115, 80], [85, 67, 25], [149, 109, 70], [55, 130, 28]),
    ("acacia", [168, 90, 50], [103, 96, 86], [150, 88, 55], [86, 120, 30]),
    ("dark_oak", [66, 43, 20], [60, 46, 26], [64, 42, 21], [51, 103, 22]),
    ("mangrove", [117, 54, 48], [84, 66, 41], [112, 54, 48], [70, 118, 38]),
    ("cherry", [226, 178, 172], [54, 33, 44], [212, 166, 158], [229, 172, 195]),
    ("pale_oak", [229, 219, 211], [88, 80, 78], [219, 207, 199], [158, 162, 150]),
    ("bamboo", [194, 173, 80], [127, 144, 58], [190, 170, 78], [90, 130, 40]),
    ("crimson", [101, 48, 70], [92, 25, 29], [119, 54, 82], [115, 3, 3]),
    ("warped", [43, 104, 99], [58, 58, 77], [52, 119, 112], [22, 119, 121]),
];

/// Flowers and other crossed plants by name, coloured by their blossom.
const PLANTS: &[(&str, [u8; 3])] = &[
    ("allium", [175, 130, 210]),
    ("azure_bluet", [214, 226, 225]),
    ("blue_orchid", [42, 170, 230]),
    ("brown_mushroom", [154, 117, 92]),
    ("cornflower", [70, 105, 220]),
    ("dandelion", [235, 215, 60]),
    ("lilac", [190, 145, 190]),
    ("lily_of_the_valley", [225, 230, 225]),
    ("orange_tulip", [235, 120, 40]),
    ("oxeye_daisy", [220, 222, 200]),
    ("peony", [225, 175, 220]),
    ("pink_tulip", [235, 175, 200]),
    ("pitcher_plant", [110, 130, 160]),
    ("poppy", [210, 40, 35]),
    ("red_mushroom", [215, 50, 45]),
    ("red_tulip", [220, 45, 40]),
    ("rose_bush", [200, 45, 40]),
    ("sunflower", [245, 200, 40]),
    ("torchflower", [235, 140, 60]),
    ("white_tulip", [230, 235, 225]),
];

/// Whether a plant is a flower, drawn with a green stem under its blossom.
pub fn is_flower(name: &str) -> bool {
    PLANTS.iter().any(|p| p.0 == name) && !name.ends_with("mushroom")
}

/// The colours of the block called `name` (without its namespace), and
/// whether the name was known rather than guessed.
pub fn lookup(name: &str) -> (Colors, bool) {
    if let Some(c) = exact(name) {
        return (c, true);
    }
    // Waxed copper looks like copper; infested stone like stone.
    for prefix in ["waxed_", "infested_"] {
        if let Some(rest) = name.strip_prefix(prefix)
            && let Some(c) = exact(rest).or_else(|| derived(rest))
        {
            return (c, true);
        }
    }
    if let Some(c) = derived(name) {
        return (c, true);
    }
    (guess(name), false)
}

fn exact(name: &str) -> Option<Colors> {
    if let Some(&(_, top, side, bottom, p)) = FACED.iter().find(|f| f.0 == name) {
        let tp = if matches!(p, Bark) { Rings } else { p };
        return Some(Colors { top: (top, tp), side: (side, p), bottom: (bottom, tp) });
    }
    if let Ok(i) = PLAIN.binary_search_by(|e| e.0.cmp(name)) {
        let (_, c, p) = PLAIN[i];
        return Some(Colors::all(c, p));
    }
    if let Some(&(_, c)) = PLANTS.iter().find(|p| p.0 == name) {
        return Some(Colors::all(c, Plant));
    }
    None
}

/// Colours worked out from a family: dyed blocks, wood, things made of
/// another block (stairs, slabs, walls, fences), ores, plants.
fn derived(name: &str) -> Option<Colors> {
    // 1. Dyed blocks.
    for (i, dye) in DYES.iter().enumerate() {
        let Some(rest) = name.strip_prefix(dye).and_then(|r| r.strip_prefix('_')) else { continue };
        let c = match rest {
            "wool" | "carpet" | "bed" | "banner" | "wall_banner" => Colors::all(WOOL[i], Leaves),
            "concrete" | "shulker_box" | "candle" | "candle_cake" => Colors::all(CONCRETE[i], Smooth),
            "concrete_powder" => Colors::all(POWDER[i], Noise),
            "terracotta" => Colors::all(TERRACOTTA[i], Noise),
            "stained_glass" | "stained_glass_pane" => Colors::all(GLASS[i], Glass),
            "glazed_terracotta" => Colors::all(CONCRETE[i], Tiles),
            _ => continue,
        };
        return Some(c);
    }
    // 2. Wood.
    for &(wood, planks, bark, ends, leaves) in WOODS {
        let rest = match name.strip_prefix("stripped_") {
            Some(r) => r.strip_prefix(wood).map(|r| (true, r)),
            None => name.strip_prefix(wood).map(|r| (false, r)),
        };
        let Some((stripped, rest)) = rest else { continue };
        let Some(rest) = rest.strip_prefix('_') else { continue };
        let stripped_side = planks.map(|v| v.saturating_add(8));
        return Some(match rest {
            "log" | "stem" if stripped => {
                Colors { top: (ends, Rings), side: (stripped_side, Bark), bottom: (ends, Rings) }
            }
            "log" | "stem" => Colors { top: (ends, Rings), side: (bark, Bark), bottom: (ends, Rings) },
            "wood" | "hyphae" if stripped => Colors::all(stripped_side, Bark),
            "wood" | "hyphae" => Colors::all(bark, Bark),
            "leaves" => Colors::all(leaves, Leaves),
            "sapling" | "propagule" | "roots" | "fungus" => Colors::all(leaves, Plant),
            "block" => Colors { top: (ends, Rings), side: (bark, Bark), bottom: (ends, Rings) },
            _ => Colors::all(planks, Planks),
        });
    }
    // 3. Things made of another block: stairs, slabs, walls and so on take
    //    the colour of what they're made of.
    const MADE: &[&str] = &[
        "_stairs",
        "_slab",
        "_wall",
        "_fence_gate",
        "_fence",
        "_pressure_plate",
        "_button",
        "_door",
        "_trapdoor",
        "_wall_hanging_sign",
        "_hanging_sign",
        "_wall_sign",
        "_sign",
        "_grate",
        "_bulb",
    ];
    for suffix in MADE {
        if let Some(base) = name.strip_suffix(suffix) {
            match base {
                "heavy_weighted" | "iron" => return exact("iron_block"),
                "light_weighted" => return exact("gold_block"),
                "petrified_oak" => return derived("oak_planks"),
                "nether_brick" => return exact("nether_bricks"),
                _ => {}
            }
            for candidate in [base.to_string(), format!("{base}s"), format!("{base}_block"), format!("{base}_planks")] {
                if let Some(c) = exact(&candidate).or_else(|| derived(&candidate)) {
                    return Some(c);
                }
            }
            // Copper's grates, bulbs, doors and trapdoors.
            if let Some(c) = exact(&format!("{base}_block")).or_else(|| exact(&format!("{base}_copper"))) {
                return Some(c);
            }
        }
    }
    if let Some(stone) = name.strip_prefix("deepslate_")
        && stone.ends_with("_ore")
    {
        let (c, _) = lookup(stone);
        let mix = |a: u8, b: u8| ((a as u16 + b as u16) / 2) as u8;
        let deep = [80, 80, 82];
        let side = c.side.0;
        return Some(Colors::all([mix(side[0], deep[0]), mix(side[1], deep[1]), mix(side[2], deep[2])], Ore));
    }
    // 4. Plants and plant parts by pattern.
    if name.ends_with("_coral") || name.ends_with("_coral_fan") || name.ends_with("_coral_wall_fan") {
        return Some(Colors::all(coral(name), Plant));
    }
    if name.ends_with("_coral_block") {
        return Some(Colors::all(coral(name), Noise));
    }
    if name.starts_with("potted_") {
        return Some(Colors::all([122, 64, 45], Smooth));
    }
    let green = |c: [u8; 3]| Some(Colors::all(c, Plant));
    match name {
        "grass" | "short_grass" | "tall_grass" | "fern" | "large_fern" | "bush" | "short_dry_grass"
        | "tall_dry_grass" => green([92, 140, 52]),
        "kelp" | "kelp_plant" | "tall_seagrass" => green([60, 120, 40]),
        "wheat"
        | "carrots"
        | "potatoes"
        | "beetroots"
        | "melon_stem"
        | "pumpkin_stem"
        | "attached_melon_stem"
        | "attached_pumpkin_stem"
        | "cave_vines"
        | "cave_vines_plant"
        | "big_dripleaf"
        | "big_dripleaf_stem"
        | "small_dripleaf"
        | "bamboo"
        | "bamboo_sapling"
        | "cocoa"
        | "glow_lichen"
        | "chorus_plant"
        | "chorus_flower"
        | "twisting_vines_plant"
        | "weeping_vines_plant"
        | "torchflower_crop"
        | "pitcher_crop" => green([96, 150, 50]),
        "chain" | "iron_chain" => Some(Colors::all([60, 66, 76], Smooth)),
        "torch"
        | "wall_torch"
        | "redstone_torch"
        | "redstone_wall_torch"
        | "soul_torch"
        | "soul_wall_torch"
        | "copper_torch"
        | "copper_wall_torch"
        | "lever" => Some(Colors::all([110, 85, 50], Smooth)),
        "rail" | "powered_rail" | "detector_rail" | "activator_rail" => Some(Colors::all([120, 105, 85], Smooth)),
        "flower_pot" => Some(Colors::all([122, 64, 45], Smooth)),
        "ender_chest" => Some(Colors::all([30, 48, 50], Smooth)),
        "skeleton_skull" | "skeleton_wall_skull" => Some(Colors::all([200, 200, 200], Smooth)),
        "wither_skeleton_skull" | "wither_skeleton_wall_skull" => Some(Colors::all([40, 40, 40], Smooth)),
        "player_head" | "player_wall_head" | "zombie_head" | "zombie_wall_head" | "creeper_head"
        | "creeper_wall_head" => Some(Colors::all([90, 120, 80], Smooth)),
        "bubble_column" => exact("water"),
        "end_portal" | "end_gateway" => Some(Colors::all([10, 12, 20], Smooth)),
        "frosted_ice" => exact("ice"),
        _ => None,
    }
}

fn coral(name: &str) -> [u8; 3] {
    let dead = name.starts_with("dead_");
    let c = if name.contains("tube") {
        [49, 87, 206]
    } else if name.contains("brain") {
        [207, 91, 159]
    } else if name.contains("bubble") {
        [165, 26, 162]
    } else if name.contains("fire") {
        [163, 35, 46]
    } else if name.contains("horn") {
        [216, 199, 66]
    } else {
        [130, 123, 119]
    };
    if dead { [130, 123, 119] } else { c }
}

/// A colour for a name the table doesn't know, from words in it, so modded
/// and future blocks still look roughly right.
fn guess(name: &str) -> Colors {
    let has = |w: &str| name.contains(w);
    let (c, p) = if has("glass") {
        ([175, 213, 219], Glass)
    } else if has("leaves") || has("leaf") {
        ([59, 118, 30], Leaves)
    } else if has("log") || has("wood") || has("plank") || has("stem") {
        ([140, 108, 66], Planks)
    } else if has("brick") {
        ([150, 97, 83], Bricks)
    } else if has("copper") {
        ([192, 107, 79], Smooth)
    } else if has("sand") {
        ([219, 207, 163], Noise)
    } else if has("dirt") || has("soil") || has("mud") {
        ([134, 96, 67], Noise)
    } else if has("grass") || has("plant") || has("flower") || has("bush") || has("vine") {
        ([92, 140, 52], Plant)
    } else if has("ore") {
        ([125, 125, 125], Ore)
    } else if has("iron") || has("steel") || has("metal") {
        ([200, 200, 200], Smooth)
    } else if has("gold") {
        ([246, 208, 61], Smooth)
    } else if has("stone") || has("rock") || has("slate") || has("cobble") {
        ([125, 125, 125], Noise)
    } else {
        ([150, 150, 150], Noise)
    };
    Colors::all(c, p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted_and_names_resolve() {
        assert!(PLAIN.windows(2).all(|w| w[0].0 < w[1].0), "PLAIN must stay sorted");
        let known = |n: &str| lookup(n).1;
        for n in [
            "stone",
            "oak_planks",
            "spruce_log",
            "stripped_birch_wood",
            "dark_oak_leaves",
            "oak_stairs",
            "stone_brick_stairs",
            "cobblestone_wall",
            "smooth_stone_slab",
            "quartz_stairs",
            "purpur_slab",
            "red_wool",
            "light_blue_concrete",
            "light_gray_stained_glass_pane",
            "black_glazed_terracotta",
            "waxed_oxidized_cut_copper_stairs",
            "deepslate_diamond_ore",
            "deepslate_tile_wall",
            "crimson_stem",
            "warped_fence_gate",
            "nether_brick_fence",
            "heavy_weighted_pressure_plate",
            "poppy",
            "tall_grass",
            "brain_coral_block",
            "grass_block",
            "infested_stone_bricks",
            "mud_brick_wall",
            "prismarine_brick_slab",
            "end_stone_brick_wall",
            "polished_blackstone_brick_stairs",
        ] {
            assert!(known(n), "{n} isn't known");
        }
        assert_eq!(lookup("light_blue_wool").0.side.0, WOOL[3]);
        assert_eq!(lookup("light_gray_wool").0.side.0, WOOL[8]);
        assert_eq!(lookup("oak_stairs").0.side.0, lookup("oak_planks").0.side.0);
        assert_eq!(lookup("stone_brick_stairs").0.side.0, lookup("stone_bricks").0.side.0);
        assert_ne!(lookup("grass_block").0.top.0, lookup("grass_block").0.side.0);
        let (unknown, known) = lookup("create:andesite_casing");
        assert!(!known && unknown.side.0 != [0, 0, 0]);
    }
}
