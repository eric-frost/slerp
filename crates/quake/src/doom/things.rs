//! Names and kinds of Doom's things (editor numbers from Doom and Doom II,
//! which Freedoom keeps), for the markers a level shows.  Heretic's and
//! Hexen's numbers mean other things, so their maps get numbers only.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Player,
    Monster,
    Weapon,
    Ammo,
    Health,
    Armor,
    Powerup,
    /// A key, with its colour: 0 blue, 1 yellow, 2 red.
    Key(u8),
    Teleport,
    Decoration,
}

/// The thing's name and kind, for Doom's editor numbers.
pub fn describe(kind: u32) -> Option<(&'static str, Kind)> {
    use Kind::*;
    Some(match kind {
        1 => ("Player 1 start", Player),
        2 => ("Player 2 start", Player),
        3 => ("Player 3 start", Player),
        4 => ("Player 4 start", Player),
        11 => ("Deathmatch start", Player),
        14 => ("Teleport destination", Teleport),
        // Monsters.
        3004 => ("Zombieman", Monster),
        9 => ("Shotgun guy", Monster),
        65 => ("Heavy weapon dude", Monster),
        3001 => ("Imp", Monster),
        3002 => ("Demon", Monster),
        58 => ("Spectre", Monster),
        3006 => ("Lost soul", Monster),
        3005 => ("Cacodemon", Monster),
        69 => ("Hell knight", Monster),
        3003 => ("Baron of Hell", Monster),
        68 => ("Arachnotron", Monster),
        71 => ("Pain elemental", Monster),
        66 => ("Revenant", Monster),
        67 => ("Mancubus", Monster),
        64 => ("Arch-vile", Monster),
        7 => ("Spider Mastermind", Monster),
        16 => ("Cyberdemon", Monster),
        84 => ("Wolfenstein SS", Monster),
        72 => ("Commander Keen", Monster),
        88 => ("Boss brain", Monster),
        89 => ("Boss shooter", Decoration),
        87 => ("Spawn spot", Decoration),
        // Weapons.
        2005 => ("Chainsaw", Weapon),
        2001 => ("Shotgun", Weapon),
        82 => ("Super shotgun", Weapon),
        2002 => ("Chaingun", Weapon),
        2003 => ("Rocket launcher", Weapon),
        2004 => ("Plasma gun", Weapon),
        2006 => ("BFG9000", Weapon),
        // Ammunition.
        2007 => ("Clip", Ammo),
        2048 => ("Box of bullets", Ammo),
        2008 => ("Shells", Ammo),
        2049 => ("Box of shells", Ammo),
        2010 => ("Rocket", Ammo),
        2046 => ("Box of rockets", Ammo),
        2047 => ("Energy cell", Ammo),
        17 => ("Energy cell pack", Ammo),
        8 => ("Backpack", Ammo),
        // Health and armour.
        2011 => ("Stimpack", Health),
        2012 => ("Medikit", Health),
        2014 => ("Health bonus", Health),
        2015 => ("Armor bonus", Armor),
        2018 => ("Armor", Armor),
        2019 => ("Mega armor", Armor),
        // Power-ups.
        2013 => ("Soulsphere", Powerup),
        83 => ("Megasphere", Powerup),
        2022 => ("Invulnerability", Powerup),
        2023 => ("Berserk", Powerup),
        2024 => ("Partial invisibility", Powerup),
        2025 => ("Radiation suit", Powerup),
        2026 => ("Computer area map", Powerup),
        2045 => ("Light amplification visor", Powerup),
        // Keys.
        5 => ("Blue keycard", Key(0)),
        40 => ("Blue skull key", Key(0)),
        6 => ("Yellow keycard", Key(1)),
        39 => ("Yellow skull key", Key(1)),
        13 => ("Red keycard", Key(2)),
        38 => ("Red skull key", Key(2)),
        // Decorations and obstacles.
        2035 => ("Barrel", Decoration),
        70 => ("Burning barrel", Decoration),
        43 => ("Burnt tree", Decoration),
        54 => ("Large brown tree", Decoration),
        47 => ("Brown stub", Decoration),
        48 => ("Tall techno column", Decoration),
        30 => ("Tall green pillar", Decoration),
        31 => ("Short green pillar", Decoration),
        32 => ("Tall red pillar", Decoration),
        33 => ("Short red pillar", Decoration),
        36 => ("Short green pillar with heart", Decoration),
        37 => ("Short red pillar with skull", Decoration),
        41 => ("Evil eye", Decoration),
        42 => ("Floating skull rock", Decoration),
        44 => ("Tall blue firestick", Decoration),
        45 => ("Tall green firestick", Decoration),
        46 => ("Tall red firestick", Decoration),
        55 => ("Short blue firestick", Decoration),
        56 => ("Short green firestick", Decoration),
        57 => ("Short red firestick", Decoration),
        34 => ("Candle", Decoration),
        35 => ("Candelabra", Decoration),
        2028 => ("Floor lamp", Decoration),
        85 => ("Tall techno floor lamp", Decoration),
        86 => ("Short techno floor lamp", Decoration),
        25 => ("Impaled human", Decoration),
        26 => ("Twitching impaled human", Decoration),
        27 => ("Skull on a pole", Decoration),
        28 => ("Five skulls shish kebab", Decoration),
        29 => ("Pile of skulls and candles", Decoration),
        10 => ("Bloody mess", Decoration),
        12 => ("Bloody mess", Decoration),
        24 => ("Pool of blood and flesh", Decoration),
        15 => ("Dead player", Decoration),
        18 => ("Dead zombieman", Decoration),
        19 => ("Dead shotgun guy", Decoration),
        20 => ("Dead imp", Decoration),
        21 => ("Dead demon", Decoration),
        22 => ("Dead cacodemon", Decoration),
        23 => ("Dead lost soul", Decoration),
        79 => ("Pool of blood", Decoration),
        80 => ("Pool of blood", Decoration),
        81 => ("Pool of brains", Decoration),
        49 | 63 => ("Hanging victim, twitching", Decoration),
        50 | 59 => ("Hanging victim, arms out", Decoration),
        51 | 61 => ("Hanging victim, one-legged", Decoration),
        52 | 60 => ("Hanging pair of legs", Decoration),
        53 | 62 => ("Hanging leg", Decoration),
        73..=78 => ("Hanging victim", Decoration),
        _ => return None,
    })
}

/// The marker colour of a kind: player starts green, monsters red, keys in
/// their colour, weapons orange, other items yellow, teleport
/// destinations violet, decorations grey.
pub fn color(kind: Option<Kind>) -> [f32; 4] {
    match kind {
        Some(Kind::Player) => [0.2, 1.0, 0.3, 1.0],
        Some(Kind::Monster) => [1.0, 0.25, 0.2, 1.0],
        Some(Kind::Key(0)) => [0.25, 0.45, 1.0, 1.0],
        Some(Kind::Key(1)) => [1.0, 0.9, 0.1, 1.0],
        Some(Kind::Key(_)) => [1.0, 0.1, 0.1, 1.0],
        Some(Kind::Weapon) => [1.0, 0.6, 0.15, 1.0],
        Some(Kind::Ammo | Kind::Health | Kind::Armor | Kind::Powerup) => [1.0, 0.85, 0.2, 1.0],
        Some(Kind::Teleport) => [0.75, 0.4, 1.0, 1.0],
        Some(Kind::Decoration) | None => [0.7, 0.7, 0.75, 1.0],
    }
}

/// The sprite a Doom thing shows standing still, by editor number: its
/// four-letter name, the frame letter, and whether it hangs from the
/// ceiling.  Things nobody sees (starts, teleport destinations, spawn
/// spots) have none.
pub fn sprite(kind: u32) -> Option<(&'static str, u8, bool)> {
    let (name, frame) = match kind {
        // Monsters.
        3004 => ("POSS", b'A'),
        9 => ("SPOS", b'A'),
        65 => ("CPOS", b'A'),
        3001 => ("TROO", b'A'),
        3002 | 58 => ("SARG", b'A'),
        3006 => ("SKUL", b'A'),
        3005 => ("HEAD", b'A'),
        69 => ("BOS2", b'A'),
        3003 => ("BOSS", b'A'),
        68 => ("BSPI", b'A'),
        71 => ("PAIN", b'A'),
        66 => ("SKEL", b'A'),
        67 => ("FATT", b'A'),
        64 => ("VILE", b'A'),
        7 => ("SPID", b'A'),
        16 => ("CYBR", b'A'),
        84 => ("SSWV", b'A'),
        72 => ("KEEN", b'A'),
        88 => ("BBRN", b'A'),
        // Weapons.
        2005 => ("CSAW", b'A'),
        2001 => ("SHOT", b'A'),
        82 => ("SGN2", b'A'),
        2002 => ("MGUN", b'A'),
        2003 => ("LAUN", b'A'),
        2004 => ("PLAS", b'A'),
        2006 => ("BFUG", b'A'),
        // Ammunition.
        2007 => ("CLIP", b'A'),
        2048 => ("AMMO", b'A'),
        2008 => ("SHEL", b'A'),
        2049 => ("SBOX", b'A'),
        2010 => ("ROCK", b'A'),
        2046 => ("BROK", b'A'),
        2047 => ("CELL", b'A'),
        17 => ("CELP", b'A'),
        8 => ("BPAK", b'A'),
        // Health, armour and powerups.
        2011 => ("STIM", b'A'),
        2012 => ("MEDI", b'A'),
        2014 => ("BON1", b'A'),
        2015 => ("BON2", b'A'),
        2018 => ("ARM1", b'A'),
        2019 => ("ARM2", b'A'),
        83 => ("MEGA", b'A'),
        2013 => ("SOUL", b'A'),
        2022 => ("PINV", b'A'),
        2023 => ("PSTR", b'A'),
        2024 => ("PINS", b'A'),
        2025 => ("SUIT", b'A'),
        2026 => ("PMAP", b'A'),
        2045 => ("PVIS", b'A'),
        // Keys.
        5 => ("BKEY", b'A'),
        6 => ("YKEY", b'A'),
        13 => ("RKEY", b'A'),
        40 => ("BSKU", b'A'),
        39 => ("YSKU", b'A'),
        38 => ("RSKU", b'A'),
        // Decorations.
        2028 => ("COLU", b'A'),
        30 => ("COL1", b'A'),
        31 => ("COL2", b'A'),
        32 => ("COL3", b'A'),
        33 => ("COL4", b'A'),
        36 => ("COL5", b'A'),
        37 => ("COL6", b'A'),
        41 => ("CEYE", b'A'),
        42 => ("FSKU", b'A'),
        43 => ("TRE1", b'A'),
        44 => ("TBLU", b'A'),
        45 => ("TGRN", b'A'),
        46 => ("TRED", b'A'),
        47 => ("SMIT", b'A'),
        48 => ("ELEC", b'A'),
        54 => ("TRE2", b'A'),
        55 => ("SMBT", b'A'),
        56 => ("SMGT", b'A'),
        57 => ("SMRT", b'A'),
        70 => ("FCAN", b'A'),
        85 => ("TLMP", b'A'),
        86 => ("TLP2", b'A'),
        34 => ("CAND", b'A'),
        35 => ("CBRA", b'A'),
        2035 => ("BAR1", b'A'),
        // Corpses and gore.
        10 | 12 => ("PLAY", b'W'),
        15 => ("PLAY", b'N'),
        18 => ("POSS", b'L'),
        19 => ("SPOS", b'L'),
        20 => ("TROO", b'M'),
        21 => ("SARG", b'N'),
        22 => ("HEAD", b'L'),
        24 => ("POL5", b'A'),
        25 => ("POL1", b'A'),
        26 => ("POL6", b'A'),
        27 => ("POL4", b'A'),
        28 => ("POL2", b'A'),
        29 => ("POL3", b'A'),
        49 | 63 => ("GOR1", b'A'),
        50 | 59 => ("GOR2", b'A'),
        51 | 61 => ("GOR3", b'A'),
        52 | 60 => ("GOR4", b'A'),
        53 | 62 => ("GOR5", b'A'),
        73 => ("HDB1", b'A'),
        74 => ("HDB2", b'A'),
        75 => ("HDB3", b'A'),
        76 => ("HDB4", b'A'),
        77 => ("HDB5", b'A'),
        78 => ("HDB6", b'A'),
        79 => ("POB1", b'A'),
        80 => ("POB2", b'A'),
        81 => ("BRS1", b'A'),
        _ => return None,
    };
    let hangs = matches!(kind, 49..=53 | 59..=63 | 72 | 73..=78);
    Some((name, frame, hangs))
}

/// Sprites the original draws at full brightness whatever the sector's
/// light: lamps, candles, torches, glowing pickups.
pub fn full_bright(name: &str) -> bool {
    matches!(
        name,
        "TLMP"
            | "TLP2"
            | "CAND"
            | "CBRA"
            | "CEYE"
            | "FCAN"
            | "TBLU"
            | "TGRN"
            | "TRED"
            | "SMBT"
            | "SMGT"
            | "SMRT"
            | "COLU"
            | "SOUL"
            | "MEGA"
            | "PINV"
            | "PINS"
            | "PVIS"
    )
}
