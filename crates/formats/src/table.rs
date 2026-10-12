//! The formats themselves.  Order matters in two small ways: generated
//! lists follow it, and the first MIME type of the thumbnailer's list names
//! Slerp's entry in Dolphin's preview settings (KIO uses that type's
//! description), so the data images stay first as they always were.

use crate::Category::*;
use crate::{Format, GNOME, KDE, MACOS, Mac, Mime, Origin, Qt, Thumbs, Uti, WINDOWS};

/*┌─────────────────╮
| Builders        |
└─────────────────╯*/
//region Builders

const fn own(name: &'static str, exts: &'static [&'static str]) -> Mime {
    Mime { name, origin: Origin::Own, exts, patterns: &[] }
}

const fn sys(name: &'static str, exts: &'static [&'static str]) -> Mime {
    Mime { name, origin: Origin::System, exts, patterns: &[] }
}

const fn extends(name: &'static str, exts: &'static [&'static str]) -> Mime {
    Mime { name, origin: Origin::Extends, exts, patterns: &[] }
}

const fn foreign(name: &'static str) -> Mime {
    Mime { name, origin: Origin::Foreign, exts: &[], patterns: &[] }
}

const fn alias(name: &'static str) -> Mime {
    Mime { name, origin: Origin::Alias, exts: &[], patterns: &[] }
}

/// A UTI Slerp imports, with the format's extensions and the category's
/// conformance.
const fn uti(id: &'static str) -> Uti {
    Uti { id, exts: &[], conforms: &[], description: "" }
}

/// A UTI Slerp imports for some of the format's extensions only.
const fn uti_for(id: &'static str, exts: &'static [&'static str]) -> Uti {
    Uti { id, exts, conforms: &[], description: "" }
}

const BASE: Format = Format {
    id: "",
    name: "",
    category: Model,
    mime: &[],
    by_content: &[],
    thumbnails: Thumbs::On,
    open_with: true,
    viewers: 0,
    qt: Qt::No,
    qt_keys: &[],
    mac: Mac::None,
    others: 0,
    better: "",
};

/// An ordinary picture: the desktops preview it, so Slerp's thumbnails are
/// off unless the user turns them on, and the viewer is offered in "Open
/// with" below the usual image viewers.
const PICTURE: Format =
    Format { category: Image, thumbnails: Thumbs::Off, others: KDE | GNOME | WINDOWS | MACOS, ..BASE };

//endregion

pub static FORMATS: &[Format] = &[
    /*┌─────────────────╮
    | Data images     |
    └─────────────────╯*/
    Format {
        id: "png-16bit",
        name: "16-bit greyscale PNG (heightmaps and masks)",
        category: Image,
        mime: &[own("image/x-png-16bit-grey", &[])],
        by_content: &["png"],
        others: KDE | GNOME | WINDOWS | MACOS,
        better: "stretches the values to their own range; other thumbnailers show heightmaps as flat black",
        ..BASE
    },
    Format {
        id: "heightmap",
        name: "Heightmap PNG (named *_height.png, *_disp.png and so on)",
        category: Image,
        mime: &[Mime {
            name: "image/x-heightmap",
            origin: Origin::Own,
            exts: &[],
            patterns: &[
                "*.height.png",
                "*_height.png",
                "*-height.png",
                "*.heightmap.png",
                "*_heightmap.png",
                "*-heightmap.png",
                "heightmap.png",
                "*_displacement.png",
                "*-displacement.png",
                "*_disp.png",
                "*_disp_*.png",
                "*.r16.png",
            ],
        }],
        qt: Qt::Still,
        qt_keys: &[("heightmap", "image/x-heightmap")],
        others: KDE | GNOME,
        better: "stretches the values to their own range; other thumbnailers show heightmaps as flat black",
        ..BASE
    },
    /*┌─────────────────╮
    | Textures        |
    └─────────────────╯*/
    Format {
        id: "ktx2",
        name: "KTX 2 texture",
        category: Texture,
        mime: &[sys("image/ktx2", &["ktx2"])],
        qt: Qt::Still,
        mac: Mac::Import(&[Uti {
            conforms: &["public.data", "public.content"],
            description: "KTX 2 texture",
            ..uti("org.khronos.ktx2")
        }]),
        ..BASE
    },
    Format {
        id: "ktx",
        name: "KTX texture",
        category: Texture,
        mime: &[sys("image/ktx", &["ktx"])],
        qt: Qt::Still,
        mac: Mac::System(&["org.khronos.ktx"]),
        others: MACOS,
        ..BASE
    },
    Format {
        id: "astc",
        name: "ASTC texture",
        category: Texture,
        mime: &[sys("image/astc", &["astc"])],
        qt: Qt::Still,
        mac: Mac::System(&["org.khronos.astc"]),
        others: MACOS,
        ..BASE
    },
    Format {
        id: "dds",
        name: "DirectDraw Surface texture (DDS)",
        category: Texture,
        mime: &[sys("image/x-dds", &["dds"]), foreign("image/vnd.ms-dds")],
        mac: Mac::System(&["com.microsoft.dds"]),
        others: KDE | GNOME | MACOS,
        better: "decodes BC4 to BC7, BC6H, ASTC and float textures, which KDE's and GNOME's thumbnailers can't",
        ..BASE
    },
    Format {
        id: "radiance-hdr",
        name: "Radiance HDR image",
        category: Image,
        mime: &[sys("image/x-hdr", &["hdr", "pic"]), foreign("image/vnd.radiance")],
        mac: Mac::System(&["public.radiance"]),
        others: MACOS,
        ..BASE
    },
    /*┌─────────────────╮
    | Models          |
    └─────────────────╯*/
    Format {
        id: "glb",
        name: "glTF binary model (GLB)",
        mime: &[sys("model/gltf-binary", &["glb"])],
        qt: Qt::Turntable,
        mac: Mac::System(&["org.khronos.glb"]),
        others: WINDOWS,
        ..BASE
    },
    Format {
        id: "gltf",
        name: "glTF model",
        mime: &[sys("model/gltf+json", &["gltf"])],
        qt: Qt::Turntable,
        mac: Mac::System(&["org.khronos.gltf"]),
        ..BASE
    },
    Format {
        id: "vrm",
        name: "VRM avatar",
        mime: &[own("model/x-vrm", &["vrm"])],
        qt: Qt::Turntable,
        mac: Mac::Import(&[Uti {
            conforms: &["org.khronos.glb", "public.3d-content", "public.data"],
            ..uti("org.vrm-consortium.vrm")
        }]),
        ..BASE
    },
    Format {
        id: "vrma",
        name: "VRM animation",
        mime: &[own("model/x-vrma", &["vrma"])],
        thumbnails: Thumbs::Never,
        ..BASE
    },
    Format {
        id: "obj",
        name: "Wavefront OBJ model",
        mime: &[sys("model/obj", &["obj"])],
        qt: Qt::Turntable,
        mac: Mac::System(&["public.geometry-definition-format"]),
        others: WINDOWS,
        ..BASE
    },
    Format {
        id: "stl",
        name: "STL model",
        mime: &[sys("model/stl", &["stl"])],
        qt: Qt::Turntable,
        mac: Mac::System(&["public.standard-tesselated-geometry-format"]),
        others: WINDOWS,
        ..BASE
    },
    Format {
        id: "fbx",
        name: "Autodesk FBX model",
        mime: &[own("application/vnd.fbx", &["fbx"])],
        qt: Qt::Turntable,
        mac: Mac::Import(&[uti("com.autodesk.fbx")]),
        others: WINDOWS,
        ..BASE
    },
    Format {
        id: "collada",
        name: "COLLADA model",
        mime: &[own("model/vnd.collada+xml", &["dae"]), foreign("application/vnd.dae")],
        qt: Qt::Turntable,
        mac: Mac::System(&["org.khronos.collada.digital-asset-exchange"]),
        ..BASE
    },
    Format {
        id: "ply",
        name: "PLY model, point cloud or splats",
        mime: &[own("application/vnd.ply", &["ply"])],
        qt: Qt::Turntable,
        mac: Mac::System(&["public.polygon-file-format"]),
        others: WINDOWS,
        ..BASE
    },
    Format {
        id: "usd",
        name: "USD scene",
        mime: &[
            own("model/vnd.usda", &["usda"]),
            own("application/vnd.usdc", &["usdc"]),
            own("application/vnd.usd", &["usd"]),
        ],
        qt: Qt::Turntable,
        mac: Mac::System(&["com.pixar.universal-scene-description", "com.pixar.universal-scene-description-utf8"]),
        others: MACOS,
        ..BASE
    },
    Format {
        id: "usdz",
        name: "USDZ package",
        mime: &[own("model/vnd.usdz+zip", &["usdz"])],
        qt: Qt::Turntable,
        mac: Mac::System(&["com.pixar.universal-scene-description-mobile"]),
        others: MACOS,
        ..BASE
    },
    Format {
        id: "vox",
        name: "MagicaVoxel model",
        mime: &[own("model/x-magicavoxel", &["vox"])],
        qt: Qt::Turntable,
        mac: Mac::Import(&[uti("io.github.ephtracy.magicavoxel")]),
        ..BASE
    },
    // One format for the four schematic layouts: users think of them as
    // "Minecraft schematics".  `.nbt` files are claimed by name, as nothing
    // can look inside gzip; the loader takes only structures (a root with
    // `size`, a palette and `blocks`), so other NBT data gets no
    // thumbnail.  The Qt plugin isn't given them yet (its content checks
    // can't see through gzip either).
    Format {
        id: "minecraft-schematic",
        name: "Minecraft schematic or structure",
        mime: &[
            own("application/x-sponge-schematic", &["schem"]),
            own("application/x-litematica-schematic", &["litematic"]),
            own("application/x-minecraft-structure", &["nbt"]),
            own("application/x-mcedit-schematic", &["schematic"]),
        ],
        mac: Mac::Import(&[
            uti_for("org.enginehub.worldedit.schem", &["schem"]),
            uti_for("fi.dy.masa.litematica.litematic", &["litematic"]),
            uti_for("com.mojang.minecraft.structure", &["nbt"]),
            uti_for("com.mcedit.schematic", &["schematic"]),
        ]),
        ..BASE
    },
    /*┌─────────────────╮
    | Roblox          |
    └─────────────────╯*/
    // Places and models, each in Roblox's binary and XML encodings, whose
    // signatures are the same for both kinds: the extension tells a place
    // from a model, and a file without one is taken for a model, the
    // general kind.  The XML types are their own so editors still see XML.
    // Roblox Studio declares no type identifiers on macOS.
    Format {
        id: "roblox-place",
        name: "Roblox place",
        category: Level,
        mime: &[own("application/x-roblox-place", &["rbxl"]), own("application/x-roblox-place+xml", &["rbxlx"])],
        mac: Mac::Import(&[
            uti_for("com.roblox.place", &["rbxl"]),
            Uti { conforms: &["public.xml", "public.data"], ..uti_for("com.roblox.place.xml", &["rbxlx"]) },
        ]),
        ..BASE
    },
    Format {
        id: "roblox-model",
        name: "Roblox model",
        mime: &[own("application/x-roblox-model", &["rbxm"]), own("application/x-roblox-model+xml", &["rbxmx"])],
        mac: Mac::Import(&[
            uti_for("com.roblox.model", &["rbxm"]),
            Uti { conforms: &["public.xml", "public.data"], ..uti_for("com.roblox.model.xml", &["rbxmx"]) },
        ]),
        ..BASE
    },
    /*┌─────────────────╮
    | Quake and Doom  |
    └─────────────────╯*/
    Format {
        id: "quake-bsp",
        name: "Quake or Half-Life map (BSP)",
        category: Level,
        mime: &[own("model/x-quake-bsp", &["bsp"])],
        qt: Qt::Turntable,
        mac: Mac::Import(&[uti("com.idsoftware.bsp")]),
        ..BASE
    },
    Format {
        id: "quake-mdl",
        name: "Quake or Half-Life model (MDL)",
        mime: &[
            own("model/x-quake-mdl", &["mdl"]),
            foreign("application/vnd.mdl"),
            own("model/x-halflife-mdl", &["mdl"]),
        ],
        qt: Qt::Turntable,
        mac: Mac::Import(&[Uti { description: "Quake or Half-Life model", ..uti("com.idsoftware.mdl") }]),
        ..BASE
    },
    // Source models share `.mdl` with Quake's and Half-Life's; the MIME
    // database tells them apart by version.  macOS knows files by
    // extension only, so `com.idsoftware.mdl` above brings Source models
    // to the Quick Look extension too, which reads the content.
    Format {
        id: "source-mdl",
        name: "Source engine model (MDL)",
        mime: &[own("model/x-source-mdl", &["mdl"])],
        ..BASE
    },
    Format {
        id: "md2",
        name: "Quake II model (MD2)",
        mime: &[own("model/x-md2", &["md2"])],
        qt: Qt::Turntable,
        mac: Mac::Import(&[Uti { description: "Quake II model", ..uti("com.idsoftware.md2") }]),
        ..BASE
    },
    Format {
        id: "md3",
        name: "Quake III model (MD3)",
        mime: &[own("model/x-md3", &["md3"])],
        qt: Qt::Turntable,
        mac: Mac::Import(&[Uti { description: "Quake III model", ..uti("com.idsoftware.md3") }]),
        ..BASE
    },
    Format {
        id: "quake-wad",
        name: "Quake or Half-Life texture WAD",
        category: Texture,
        mime: &[own("application/x-quake-wad", &["wad"])],
        mac: Mac::Import(&[Uti {
            conforms: &["public.data", "public.content"],
            description: "Quake, Half-Life or Doom WAD",
            ..uti("com.idsoftware.wad")
        }]),
        ..BASE
    },
    Format {
        id: "doom-wad",
        name: "Doom WAD",
        category: Level,
        mime: &[sys("application/x-doom-wad", &["wad"])],
        mac: Mac::Import(&[Uti {
            conforms: &["public.data", "public.content"],
            description: "Quake, Half-Life or Doom WAD",
            ..uti("com.idsoftware.wad")
        }]),
        ..BASE
    },
    /*┌─────────────────╮
    | Materials       |
    └─────────────────╯*/
    Format {
        id: "materialx",
        name: "MaterialX material",
        category: Material,
        mime: &[own("application/x-materialx", &["mtlx"])],
        qt: Qt::Turntable,
        mac: Mac::Import(&[Uti { conforms: &["public.xml", "public.data"], ..uti("org.materialx.mtlx") }]),
        ..BASE
    },
    Format {
        id: "godot-resource",
        name: "Godot material or mesh resource",
        category: Material,
        // Only .tres: Windows calls compiled resource scripts .res.
        mime: &[sys("application/x-godot-resource", &["tres"])],
        qt: Qt::Turntable,
        mac: Mac::Import(&[Uti {
            exts: &["tres"],
            conforms: &["public.data"],
            description: "Godot Engine resource",
            ..uti("public.res")
        }]),
        ..BASE
    },
    Format {
        id: "sbsar",
        name: "Substance material",
        category: Material,
        mime: &[own("application/x-substance-archive", &["sbsar"])],
        mac: Mac::Import(&[Uti { conforms: &["public.data", "public.content"], ..uti("com.adobe.substance.sbsar") }]),
        ..BASE
    },
    Format {
        id: "uasset",
        name: "Unreal Engine asset",
        category: Preview,
        mime: &[own("application/x-unreal-asset", &["uasset"])],
        mac: Mac::Import(&[Uti { conforms: &["public.data", "public.content"], ..uti("com.epicgames.unreal.uasset") }]),
        ..BASE
    },
    Format {
        id: "metahuman-dna",
        name: "MetaHuman DNA rig",
        mime: &[own("application/x-metahuman-dna", &["dna"])],
        qt: Qt::Turntable,
        mac: Mac::Import(&[uti("com.epicgames.metahuman.dna")]),
        ..BASE
    },
    /*┌─────────────────╮
    | Splats          |
    └─────────────────╯*/
    Format {
        id: "splat",
        name: "Gaussian splats (.splat)",
        category: PointCloud,
        mime: &[own("application/vnd.splat", &["splat"])],
        mac: Mac::Import(&[uti("io.github.antimatter15.splat")]),
        ..BASE
    },
    Format {
        id: "spz",
        name: "Compressed Gaussian splats (SPZ)",
        category: PointCloud,
        mime: &[own("application/vnd.spz", &["spz"])],
        mac: Mac::Import(&[uti("com.nianticlabs.spz")]),
        ..BASE
    },
    Format {
        id: "sog",
        name: "PlayCanvas splats (SOG)",
        category: PointCloud,
        mime: &[own("application/x-sog", &["sog"])],
        mac: Mac::Import(&[uti("com.playcanvas.sog")]),
        ..BASE
    },
    /*┌─────────────────╮
    | Sprites, LUTs   |
    └─────────────────╯*/
    Format {
        id: "aseprite",
        name: "Aseprite sprite",
        category: Sprite,
        mime: &[own("image/x-aseprite", &["aseprite", "ase"])],
        qt: Qt::Still,
        mac: Mac::Import(&[uti("org.aseprite.sprite")]),
        ..BASE
    },
    Format {
        id: "tiled",
        name: "Tiled map",
        category: Sprite,
        mime: &[sys("application/x-tiled-tmx", &["tmx"]), own("application/x-tiled-tmj", &["tmj"])],
        qt: Qt::Still,
        mac: Mac::Import(&[uti_for("org.mapeditor.tmx", &["tmx"]), uti_for("org.mapeditor.tmj", &["tmj"])]),
        ..BASE
    },
    Format {
        id: "cube-lut",
        name: "Colour lookup table (Cube)",
        category: Colour,
        mime: &[own("application/x-cube-lut", &["cube"])],
        mac: Mac::Import(&[uti("com.adobe.cube-lut")]),
        ..BASE
    },
    Format {
        id: "3dl-lut",
        name: "Colour lookup table (3DL)",
        category: Colour,
        mime: &[own("application/x-3dl-lut", &["3dl"])],
        mac: Mac::Import(&[uti("com.autodesk.3dl-lut")]),
        ..BASE
    },
    /*┌─────────────────╮
    | More models     |
    └─────────────────╯*/
    Format {
        id: "3mf",
        name: "3MF model",
        mime: &[sys("model/3mf", &["3mf"])],
        qt: Qt::Turntable,
        mac: Mac::Import(&[uti("org.3mf.3mf")]),
        others: KDE | WINDOWS,
        ..BASE
    },
    Format {
        id: "off",
        name: "OFF mesh",
        mime: &[own("application/vnd.off", &["off"])],
        qt: Qt::Turntable,
        mac: Mac::Import(&[uti("edu.princeton.off")]),
        ..BASE
    },
    Format {
        id: "3ds",
        name: "3D Studio mesh (3DS)",
        mime: &[sys("image/x-3ds", &["3ds"]), foreign("application/vnd.3ds")],
        qt: Qt::Turntable,
        mac: Mac::Import(&[uti("com.autodesk.3ds")]),
        ..BASE
    },
    Format {
        id: "directx",
        name: "DirectX model (.x)",
        mime: &[own("application/vnd.x", &["x"])],
        qt: Qt::Turntable,
        mac: Mac::Import(&[uti("com.microsoft.directx.x")]),
        ..BASE
    },
    Format {
        id: "vrml",
        name: "VRML world",
        mime: &[extends("model/vrml", &["wrl", "vrml", "wrz"])],
        qt: Qt::Turntable,
        mac: Mac::Import(&[uti("org.web3d.vrml")]),
        ..BASE
    },
    Format {
        id: "x3d",
        name: "X3D scene",
        mime: &[own("model/x3d+xml", &["x3d", "x3dz"]), own("model/x3d+vrml", &["x3dv", "x3dvz"])],
        qt: Qt::Turntable,
        mac: Mac::Import(&[uti("org.web3d.x3d")]),
        ..BASE
    },
    /*┌─────────────────╮
    | Rigged models   |
    └─────────────────╯*/
    Format {
        id: "draco",
        name: "Draco geometry",
        // F3D's name for it.  No glob: Delphi's resource string files are
        // .drc too.
        mime: &[own("application/vnd.drc", &[])],
        by_content: &["drc"],
        ..BASE
    },
    Format {
        id: "mmd-pmx",
        name: "MikuMikuDance model (PMX)",
        mime: &[own("model/x-mmd-pmx", &["pmx"])],
        mac: Mac::Import(&[Uti { description: "MikuMikuDance model", ..uti("jp.vpvp.mmd.pmx") }]),
        ..BASE
    },
    Format {
        id: "mmd-pmd",
        name: "MikuMikuDance model (PMD)",
        // A weight-40 glob under magic: PageMaker documents are .pmd too.
        mime: &[own("model/x-mmd-pmd", &["pmd"])],
        ..BASE
    },
    Format {
        id: "mmd-vmd",
        name: "MikuMikuDance motion (VMD)",
        mime: &[own("model/x-mmd-vmd", &["vmd"])],
        thumbnails: Thumbs::Never,
        mac: Mac::Import(&[Uti { description: "MikuMikuDance motion", ..uti("jp.vpvp.mmd.vmd") }]),
        ..BASE
    },
    Format {
        id: "md5mesh",
        name: "Doom 3 model (MD5)",
        mime: &[own("model/x-md5mesh", &["md5mesh"])],
        mac: Mac::Import(&[Uti { description: "Doom 3 model", ..uti("com.idsoftware.md5mesh") }]),
        ..BASE
    },
    Format {
        id: "md5anim",
        name: "Doom 3 animation (MD5)",
        mime: &[own("model/x-md5anim", &["md5anim"])],
        thumbnails: Thumbs::Never,
        mac: Mac::Import(&[Uti { description: "Doom 3 animation", ..uti("com.idsoftware.md5anim") }]),
        ..BASE
    },
    Format {
        id: "iqm",
        name: "Inter-Quake Model (IQM)",
        mime: &[own("model/x-iqm", &["iqm"])],
        mac: Mac::Import(&[Uti { description: "Inter-Quake Model", ..uti("org.sauerbraten.iqm") }]),
        ..BASE
    },
    Format {
        id: "iqe",
        name: "Inter-Quake Export (IQE)",
        mime: &[own("model/x-iqe", &["iqe"])],
        mac: Mac::Import(&[Uti { description: "Inter-Quake Export", ..uti("org.sauerbraten.iqe") }]),
        ..BASE
    },
    Format {
        id: "source-smd",
        name: "Source engine model source (SMD)",
        // A weight-40 glob under magic: Mega Drive ROMs and StarMail mail
        // are .smd too.
        mime: &[own("model/x-source-smd", &["smd"])],
        ..BASE
    },
    Format {
        id: "unreal-psk",
        name: "Unreal skeletal mesh (PSK)",
        mime: &[own("model/x-unreal-psk", &["psk", "pskx"])],
        mac: Mac::Import(&[Uti { description: "Unreal skeletal mesh", ..uti("com.epicgames.psk") }]),
        ..BASE
    },
    Format {
        id: "unreal-psa",
        name: "Unreal animation set (PSA)",
        // No glob: Photoshop Album catalogues are .psa too.
        mime: &[own("model/x-unreal-psa", &[])],
        by_content: &["psa"],
        thumbnails: Thumbs::Never,
        ..BASE
    },
    /*┌─────────────────╮
    | Point clouds    |
    └─────────────────╯*/
    Format {
        id: "las",
        name: "LAS or LAZ lidar point cloud",
        category: PointCloud,
        mime: &[own("application/vnd.las", &["las"]), own("application/vnd.laszip", &["laz"])],
        qt: Qt::Turntable,
        mac: Mac::Import(&[uti_for("org.asprs.las", &["las"]), uti_for("org.asprs.laz", &["laz"])]),
        ..BASE
    },
    Format {
        id: "e57",
        name: "E57 laser scan",
        category: PointCloud,
        mime: &[own("model/e57", &["e57"])],
        qt: Qt::Turntable,
        mac: Mac::Import(&[uti("org.astm.e57")]),
        ..BASE
    },
    Format {
        id: "pcd",
        name: "PCD point cloud",
        category: PointCloud,
        mime: &[own("application/vnd.pcd", &["pcd"])],
        qt: Qt::Turntable,
        mac: Mac::Import(&[uti("org.pointclouds.pcd")]),
        ..BASE
    },
    Format {
        id: "pts",
        name: "PTS point cloud",
        category: PointCloud,
        mime: &[own("application/vnd.pts", &["pts"])],
        qt: Qt::Turntable,
        mac: Mac::Import(&[uti("com.leica-geosystems.pts")]),
        ..BASE
    },
    Format {
        id: "ptx",
        name: "PTX laser scan",
        category: PointCloud,
        mime: &[own("application/vnd.ptx", &["ptx"])],
        qt: Qt::Turntable,
        mac: Mac::Import(&[uti("com.leica-geosystems.ptx")]),
        ..BASE
    },
    /*┌─────────────────╮
    | Levels, scenes  |
    └─────────────────╯*/
    Format {
        id: "doom-pk3",
        name: "Doom resource archive (PK3)",
        category: Level,
        mime: &[own("application/x-doom-pk3", &["pk3", "ipk3", "pke"])],
        mac: Mac::Import(&[Uti { conforms: &["public.zip-archive", "public.data"], ..uti("org.zdoom.pk3") }]),
        ..BASE
    },
    Format {
        id: "doom-pk7",
        name: "Doom resource archive (PK7)",
        category: Level,
        mime: &[own("application/x-doom-pk7", &["pk7", "ipk7"])],
        mac: Mac::Import(&[uti("org.zdoom.pk7")]),
        ..BASE
    },
    Format {
        id: "ldtk",
        name: "LDtk project or level",
        category: Sprite,
        mime: &[own("application/x-ldtk", &["ldtk"]), own("application/x-ldtk-level", &["ldtkl"])],
        qt: Qt::Still,
        mac: Mac::Import(&[uti_for("io.deepnight.ldtk", &["ldtk"]), uti_for("io.deepnight.ldtkl", &["ldtkl"])]),
        ..BASE
    },
    Format {
        id: "blockbench",
        name: "Blockbench model",
        mime: &[own("model/vnd.blockbench.bbmodel", &["bbmodel"])],
        qt: Qt::Turntable,
        mac: Mac::Import(&[uti("net.blockbench.bbmodel")]),
        ..BASE
    },
    // The scene opens in the viewer; thumbnails stay the picture 3ds Max
    // saved in the file.
    Format {
        id: "3dsmax",
        name: "3ds Max scene",
        category: Model,
        mime: &[own("application/x-3dsmax", &["max"])],
        mac: Mac::Import(&[uti("com.autodesk.3dsmax")]),
        ..BASE
    },
    // The model opens in the viewer; thumbnails stay the picture SketchUp
    // saved in the file.
    Format {
        id: "sketchup",
        name: "SketchUp model",
        category: Model,
        mime: &[own("application/vnd.sketchup.skp", &["skp"])],
        mac: Mac::Import(&[uti("com.sketchup.skp")]),
        ..BASE
    },
    // Maya scenes.  Mathematica notebooks and others use the extensions
    // too, so they are claimed by content alone.
    Format {
        id: "maya",
        name: "Maya scene",
        mime: &[own("application/x-maya-ascii", &[]), own("application/x-maya-binary", &[])],
        by_content: &["ma", "mb"],
        ..BASE
    },
    Format {
        id: "cinema4d",
        name: "Cinema 4D scene",
        category: Preview,
        mime: &[own("application/x-c4d", &["c4d"])],
        mac: Mac::Import(&[uti("com.maxon.c4d")]),
        ..BASE
    },
    Format {
        id: "zbrush",
        name: "ZBrush tool, project or document",
        category: Preview,
        mime: &[
            own("application/x-zbrush-tool", &["ztl"]),
            own("application/x-zbrush-project", &["zpr"]),
            own("application/x-zbrush-document", &["zbr"]),
        ],
        mac: Mac::Import(&[
            uti_for("com.pixologic.zbrush.ztl", &["ztl"]),
            uti_for("com.pixologic.zbrush.zpr", &["zpr"]),
            uti_for("com.pixologic.zbrush.zbr", &["zbr"]),
        ]),
        ..BASE
    },
    Format {
        id: "godot-scene",
        name: "Godot scene",
        category: Scene,
        mime: &[sys("application/x-godot-scene", &["tscn", "scn", "escn"])],
        qt: Qt::Turntable,
        qt_keys: &[("tscn", "application/x-godot-scene"), ("escn", "application/x-godot-scene")],
        // macOS gives .scn to SceneKit.
        mac: Mac::Import(&[uti_for("org.godotengine.scene", &["tscn", "escn"])]),
        ..BASE
    },
    Format {
        id: "unity",
        name: "Unity scene or prefab",
        category: Scene,
        mime: &[own("application/x-unity-scene", &["unity"]), own("application/x-unity-prefab", &["prefab"])],
        qt: Qt::Turntable,
        mac: Mac::Import(&[uti_for("com.unity3d.scene", &["unity"]), uti_for("com.unity3d.prefab", &["prefab"])]),
        ..BASE
    },
    Format {
        id: "blend",
        name: "Blender file",
        category: Scene,
        mime: &[sys("application/x-blender", &["blend", "blender"])],
        qt: Qt::Turntable,
        qt_keys: &[("blend", "application/x-blender")],
        mac: Mac::Import(&[uti_for("org.blenderfoundation.blender.file", &["blend"])]),
        others: KDE,
        ..BASE
    },
    Format {
        id: "quake-map",
        name: "Quake or Half-Life map source (.map)",
        category: Level,
        mime: &[own("model/x-quake-map", &[])],
        by_content: &["map"],
        qt: Qt::Turntable,
        qt_keys: &[("map", "model/x-quake-map")],
        ..BASE
    },
    Format {
        id: "valve-vmf",
        name: "Hammer map source (VMF)",
        category: Level,
        mime: &[own("model/x-valve-vmf", &["vmf"])],
        qt: Qt::Turntable,
        mac: Mac::Import(&[Uti { conforms: &["public.plain-text", "public.data"], ..uti("com.valvesoftware.vmf") }]),
        ..BASE
    },
    /*┌─────────────────╮
    | Creative apps   |
    └─────────────────╯*/
    Format {
        id: "procreate",
        name: "Procreate artwork",
        category: Preview,
        mime: &[own("application/x-procreate", &["procreate"])],
        mac: Mac::Import(&[uti("com.savage.procreate")]),
        ..BASE
    },
    Format {
        id: "affinity",
        name: "Affinity document",
        category: Preview,
        mime: &[
            own("application/x-affinity-photo", &["afphoto"]),
            own("application/x-affinity-designer", &["afdesign"]),
            own("application/x-affinity-publisher", &["afpub"]),
            own("application/x-affinity", &["af"]),
        ],
        mac: Mac::Import(&[
            uti_for("com.seriflabs.affinityphoto.document", &["afphoto"]),
            uti_for("com.seriflabs.affinitydesigner.document", &["afdesign"]),
            uti_for("com.seriflabs.affinitypublisher.document", &["afpub"]),
            uti_for("com.seriflabs.affinity.document", &["af"]),
        ]),
        ..BASE
    },
    Format {
        id: "sketch",
        name: "Sketch document",
        category: Preview,
        mime: &[own("application/x-sketch", &["sketch"])],
        mac: Mac::Import(&[uti("com.bohemiancoding.sketch.drawing.single")]),
        ..BASE
    },
    Format {
        id: "figma",
        name: "Figma file",
        category: Preview,
        mime: &[own("application/x-figma", &["fig"])],
        mac: Mac::Import(&[uti("com.figma.document")]),
        ..BASE
    },
    Format {
        id: "clip-studio",
        name: "Clip Studio Paint illustration",
        category: Preview,
        mime: &[own("application/x-clip-studio", &["clip"])],
        mac: Mac::Import(&[uti("jp.co.celsys.clipstudio.clip")]),
        ..BASE
    },
    Format {
        id: "paintnet",
        name: "Paint.NET image",
        category: Preview,
        mime: &[own("image/x-paintnet", &["pdn"])],
        mac: Mac::Import(&[uti("com.getpaint.pdn")]),
        ..BASE
    },
    Format {
        id: "indesign",
        name: "InDesign document",
        category: Preview,
        mime: &[own("application/x-adobe-indesign", &["indd"])],
        mac: Mac::Import(&[uti("com.adobe.indesign-document")]),
        ..BASE
    },
    Format {
        id: "adobe-xd",
        name: "Adobe XD document",
        category: Preview,
        mime: &[own("application/vnd.adobe.sparkler.project+dcxucf", &["xd"])],
        mac: Mac::Import(&[uti("com.adobe.xd.document")]),
        ..BASE
    },
    Format {
        id: "freecad",
        name: "FreeCAD document",
        category: Preview,
        mime: &[own("application/x-extension-fcstd", &["fcstd"])],
        mac: Mac::Import(&[uti("org.freecad.fcstd")]),
        ..BASE
    },
    /*┌─────────────────╮
    | Palettes        |
    └─────────────────╯*/
    Format {
        id: "ase-palette",
        name: "Adobe Swatch Exchange palette (.ase)",
        category: Colour,
        mime: &[own("application/x-adobe-ase", &["ase"])],
        mac: Mac::Import(&[uti("com.adobe.ase")]),
        ..BASE
    },
    Format {
        id: "aco",
        name: "Photoshop swatches (.aco)",
        category: Colour,
        mime: &[own("application/x-adobe-aco", &["aco"])],
        mac: Mac::Import(&[uti("com.adobe.photoshop.aco")]),
        ..BASE
    },
    Format {
        id: "act",
        name: "Adobe colour table (.act)",
        category: Colour,
        mime: &[own("application/x-adobe-act", &["act"])],
        mac: Mac::Import(&[uti("com.adobe.act")]),
        ..BASE
    },
    Format {
        id: "gpl",
        name: "GIMP palette (.gpl)",
        category: Colour,
        mime: &[own("application/x-gimp-palette", &["gpl"])],
        mac: Mac::Import(&[Uti { conforms: &["public.plain-text", "public.data"], ..uti("org.gimp.gpl") }]),
        ..BASE
    },
    Format {
        id: "kpl",
        name: "Krita palette (.kpl)",
        category: Colour,
        mime: &[own("application/x-krita-palette", &["kpl"])],
        mac: Mac::Import(&[uti("org.krita.kpl")]),
        ..BASE
    },
    Format {
        id: "jasc-pal",
        name: "JASC palette (.pal)",
        category: Colour,
        mime: &[own("application/x-jasc-palette", &[])],
        by_content: &["pal"],
        ..BASE
    },
    Format {
        id: "riff-pal",
        name: "RIFF palette (.pal)",
        category: Colour,
        mime: &[own("application/x-riff-palette", &[])],
        by_content: &["pal"],
        ..BASE
    },
    Format {
        id: "hex-palette",
        name: "Hex colour list (.hex)",
        category: Colour,
        mime: &[own("application/x-hex-palette", &["hex"])],
        // Intel HEX firmware owns .hex on macOS and Windows, which can't
        // tell them apart by content.
        ..BASE
    },
    Format {
        id: "procreate-swatches",
        name: "Procreate swatches",
        category: Colour,
        mime: &[own("application/x-procreate-swatches", &["swatches"])],
        mac: Mac::Import(&[uti("com.savage.procreate.swatches")]),
        ..BASE
    },
    /*┌─────────────────╮
    | Paths           |
    └─────────────────╯*/
    Format {
        id: "gcode",
        name: "G-code toolpath",
        category: Path,
        mime: &[extends("text/x.gcode", &["gcode", "gco"])],
        by_content: &["g", "gc", "nc", "ngc", "tap", "cnc"],
        mac: Mac::Import(&[Uti { conforms: &["public.plain-text", "public.data"], ..uti("org.reprap.gcode") }]),
        ..BASE
    },
    Format {
        id: "bgcode",
        name: "Binary G-code",
        category: Path,
        mime: &[own("application/x-bgcode", &["bgcode"])],
        mac: Mac::Import(&[uti("com.prusa3d.bgcode")]),
        ..BASE
    },
    Format {
        id: "gpx",
        name: "GPS track (GPX)",
        category: Path,
        mime: &[sys("application/gpx+xml", &["gpx"])],
        mac: Mac::Import(&[Uti { conforms: &["public.xml", "public.data"], ..uti("com.topografix.gpx") }]),
        ..BASE
    },
    Format {
        id: "tcx",
        name: "Garmin activity (TCX)",
        category: Path,
        mime: &[own("application/vnd.garmin.tcx+xml", &["tcx"])],
        mac: Mac::Import(&[Uti { conforms: &["public.xml", "public.data"], ..uti("com.garmin.tcx") }]),
        ..BASE
    },
    Format {
        id: "fit",
        name: "Activity file (FIT)",
        category: Path,
        mime: &[own("application/vnd.ant.fit", &["fit"])],
        mac: Mac::Import(&[uti("com.garmin.fit")]),
        ..BASE
    },
    Format {
        id: "kml",
        name: "Map data (KML or KMZ)",
        category: Path,
        mime: &[
            sys("application/vnd.google-earth.kml+xml", &["kml"]),
            sys("application/vnd.google-earth.kmz", &["kmz"]),
        ],
        mac: Mac::Import(&[
            Uti { exts: &["kml"], conforms: &["public.xml", "public.data"], ..uti("com.google.earth.kml") },
            Uti { exts: &["kmz"], conforms: &["public.zip-archive", "public.data"], ..uti("com.google.earth.kmz") },
        ]),
        ..BASE
    },
    Format {
        id: "geojson",
        name: "GeoJSON",
        category: Path,
        mime: &[sys("application/geo+json", &["geojson", "geo.json"])],
        mac: Mac::System(&["public.geojson"]),
        ..BASE
    },
    /*┌─────────────────╮
    | Medical         |
    └─────────────────╯*/
    Format {
        id: "dicom",
        name: "DICOM medical image",
        category: Medical,
        mime: &[sys("application/dicom", &["dcm"])],
        by_content: &["dicom"],
        mac: Mac::System(&["org.nema.dicom"]),
        ..BASE
    },
    /*┌─────────────────╮
    | Drawings        |
    └─────────────────╯*/
    Format {
        id: "dxf",
        name: "AutoCAD drawing (DXF)",
        category: Drawing,
        mime: &[extends("image/vnd.dxf", &["dxf"])],
        mac: Mac::Import(&[uti("com.autodesk.dxf")]),
        ..BASE
    },
    Format {
        id: "rhino",
        name: "Rhino model (3DM)",
        category: Drawing,
        mime: &[own("model/x-3dm", &["3dm"]), alias("model/vnd.3dm"), alias("application/x-3dm")],
        mac: Mac::Import(&[Uti {
            conforms: &["public.3d-content", "public.data"],
            ..uti("com.mcneel.rhinoceros.3dm")
        }]),
        ..BASE
    },
    /*┌─────────────────╮
    | Retro and games |
    └─────────────────╯*/
    Format {
        id: "atari-degas",
        name: "Atari ST DEGAS picture",
        category: Retro,
        mime: &[own("image/x-atari-degas", &["pi1", "pi2", "pi3", "pc1", "pc2", "pc3"])],
        qt: Qt::Still,
        mac: Mac::Import(&[uti("com.atari.st.degas")]),
        ..BASE
    },
    Format {
        id: "atari-neochrome",
        name: "Atari ST NEOchrome picture",
        category: Retro,
        mime: &[own("image/x-atari-neochrome", &["neo"])],
        qt: Qt::Still,
        mac: Mac::Import(&[uti("com.atari.st.neochrome")]),
        ..BASE
    },
    Format {
        id: "atari-tiny",
        name: "Atari ST Tiny Stuff picture",
        category: Retro,
        mime: &[own("image/x-atari-tiny", &["tn1", "tn2", "tn3", "tny"])],
        qt: Qt::Still,
        mac: Mac::Import(&[uti("com.atari.st.tiny")]),
        ..BASE
    },
    Format {
        id: "c64-koala",
        name: "Commodore 64 Koala Painter picture",
        category: Retro,
        mime: &[own("image/x-c64-koala", &["koa", "kla"])],
        qt: Qt::Still,
        mac: Mac::Import(&[uti("com.commodore.c64.koala")]),
        ..BASE
    },
    Format {
        id: "c64-artstudio",
        name: "Commodore 64 Art Studio picture",
        category: Retro,
        mime: &[own("image/x-c64-artstudio", &["art", "aas"])],
        qt: Qt::Still,
        // .art is the Build engine's too; macOS can't tell them apart.
        mac: Mac::Import(&[uti_for("com.commodore.c64.artstudio", &["aas"])]),
        ..BASE
    },
    Format {
        id: "c64-doodle",
        name: "Commodore 64 Doodle picture",
        category: Retro,
        mime: &[own("image/x-c64-doodle", &["dd", "jj"])],
        qt: Qt::Still,
        mac: Mac::Import(&[uti_for("com.commodore.c64.doodle", &["jj"])]),
        ..BASE
    },
    Format {
        id: "c64-hires",
        name: "Commodore 64 hires bitmap",
        category: Retro,
        mime: &[own("image/x-c64-hires", &["hir", "hpi", "ocp"])],
        qt: Qt::Still,
        mac: Mac::Import(&[uti("com.commodore.c64.hires")]),
        ..BASE
    },
    Format {
        id: "msx",
        name: "MSX screen dump",
        category: Retro,
        mime: &[own(
            "image/x-msx-screen",
            &["sc2", "sc5", "sc6", "sc7", "sc8", "sc12", "sr5", "sr6", "sr7", "sr8", "sr12", "s12", "srs"],
        )],
        qt: Qt::Still,
        mac: Mac::Import(&[uti("org.msx.screen")]),
        ..BASE
    },
    Format {
        id: "apple2-hires",
        name: "Apple II hi-res picture",
        category: Retro,
        mime: &[own("image/x-apple2-hires", &["hgr"])],
        qt: Qt::Still,
        mac: Mac::Import(&[uti("com.apple.apple2.hgr")]),
        ..BASE
    },
    Format {
        id: "apple2-double-hires",
        name: "Apple II double hi-res picture",
        category: Retro,
        mime: &[own("image/x-apple2-double-hires", &["dhr"])],
        qt: Qt::Still,
        mac: Mac::Import(&[uti("com.apple.apple2.dhr")]),
        ..BASE
    },
    Format {
        id: "atari8",
        name: "Atari 8-bit picture",
        category: Retro,
        mime: &[own("image/x-atari8-screen", &["mic", "g15", "gr15", "gr9", "gr8"])],
        qt: Qt::Still,
        mac: Mac::Import(&[uti("com.atari.8bit.screen")]),
        ..BASE
    },
    Format {
        id: "bbc",
        name: "BBC Micro screen dump",
        category: Retro,
        mime: &[own("image/x-bbc-screen", &["bbc"])],
        qt: Qt::Still,
        mac: Mac::Import(&[uti("uk.co.acorn.bbc.screen")]),
        ..BASE
    },
    Format {
        id: "ansi-art",
        name: "ANSI art",
        category: Retro,
        mime: &[own("text/x-ansi-art", &["ans"])],
        qt: Qt::Still,
        mac: Mac::Import(&[Uti { conforms: &["public.plain-text", "public.data"], ..uti("org.sixteencolors.ansi") }]),
        ..BASE
    },
    Format {
        id: "pico8",
        name: "PICO-8 cartridge",
        category: Retro,
        mime: &[own("application/x-pico8-cartridge", &["p8"])],
        qt: Qt::Still,
        mac: Mac::Import(&[Uti {
            conforms: &["public.plain-text", "public.data"],
            ..uti("com.lexaloffle.pico-8.cart")
        }]),
        ..BASE
    },
    Format {
        id: "tic80",
        name: "TIC-80 cartridge",
        category: Retro,
        mime: &[own("application/x-tic80-cartridge", &["tic"])],
        qt: Qt::Still,
        mac: Mac::Import(&[uti("com.nesbox.tic80.cart")]),
        ..BASE
    },
    Format {
        id: "xnb",
        name: "XNA or MonoGame texture (XNB)",
        category: Texture,
        mime: &[own("application/x-xnb", &["xnb"])],
        qt: Qt::Still,
        mac: Mac::Import(&[uti("com.microsoft.xna.xnb")]),
        ..BASE
    },
    Format {
        id: "blp",
        name: "Blizzard texture (BLP)",
        category: Texture,
        mime: &[own("image/x-blp", &["blp"])],
        qt: Qt::Still,
        mac: Mac::Import(&[uti("com.blizzard.blp")]),
        ..BASE
    },
    Format {
        id: "pvr",
        name: "PowerVR texture (PVR)",
        category: Texture,
        mime: &[own("image/x-pvr", &["pvr"])],
        qt: Qt::Still,
        mac: Mac::System(&["public.pvr"]),
        others: MACOS,
        ..BASE
    },
    Format {
        id: "pkm",
        name: "Ericsson ETC texture (PKM)",
        category: Texture,
        mime: &[own("image/x-pkm", &["pkm"])],
        qt: Qt::Still,
        mac: Mac::Import(&[uti("com.ericsson.pkm")]),
        ..BASE
    },
    Format {
        id: "build-art",
        name: "Build engine tiles (ART)",
        category: Texture,
        mime: &[own("image/x-build-art", &["art"])],
        qt: Qt::Still,
        ..BASE
    },
    Format {
        id: "quake-sprite",
        name: "Quake or Half-Life sprite (SPR)",
        category: Sprite,
        mime: &[own("image/x-quake-sprite", &["spr"])],
        qt: Qt::Still,
        mac: Mac::Import(&[uti("com.idsoftware.spr")]),
        ..BASE
    },
    Format {
        id: "quake2-sprite",
        name: "Quake II sprite (SP2)",
        category: Sprite,
        mime: &[own("image/x-quake2-sprite", &["sp2"])],
        qt: Qt::Still,
        mac: Mac::Import(&[uti("com.idsoftware.sp2")]),
        ..BASE
    },
    Format {
        id: "libgdx-atlas",
        name: "libGDX or Spine texture atlas",
        category: Sprite,
        mime: &[own("application/x-libgdx-atlas", &["atlas"])],
        mac: Mac::Import(&[Uti {
            conforms: &["public.plain-text", "public.data"],
            ..uti("com.badlogicgames.gdx.atlas")
        }]),
        ..BASE
    },
    Format {
        id: "texturepacker",
        name: "TexturePacker sprite sheet data",
        category: Sprite,
        mime: &[own("application/x-texturepacker-sheet", &["tpsheet"])],
        mac: Mac::Import(&[Uti { conforms: &["public.plain-text", "public.data"], ..uti("com.codeandweb.tpsheet") }]),
        ..BASE
    },
    Format {
        id: "ilbm",
        name: "Amiga IFF picture",
        category: Retro,
        mime: &[sys("image/x-ilbm", &["iff", "ilbm", "lbm"])],
        mac: Mac::Import(&[uti("com.ea.iff-ilbm")]),
        ..BASE
    },
    /*┌─────────────────╮
    | Metafiles       |
    └─────────────────╯*/
    Format {
        id: "wmf",
        name: "Windows Metafile (WMF)",
        category: Drawing,
        mime: &[extends("image/wmf", &["wmf"]), own("image/x-compressed-wmf", &["wmz"])],
        mac: Mac::Import(&[uti_for("com.microsoft.wmf", &["wmf"]), uti_for("com.microsoft.wmz", &["wmz"])]),
        others: WINDOWS,
        ..BASE
    },
    Format {
        id: "emf",
        name: "Enhanced Metafile (EMF)",
        category: Drawing,
        mime: &[extends("image/emf", &["emf"]), own("image/x-compressed-emf", &["emz"])],
        mac: Mac::Import(&[uti_for("com.microsoft.emf", &["emf"]), uti_for("com.microsoft.emz", &["emz"])]),
        others: WINDOWS,
        ..BASE
    },
    Format {
        id: "pict",
        name: "Macintosh PICT picture",
        category: Drawing,
        mime: &[extends("image/x-pict", &["pict", "pct", "pict1", "pict2"])],
        mac: Mac::System(&["com.apple.pict"]),
        others: MACOS,
        ..BASE
    },
    /*┌─────────────────╮
    | Documents       |
    └─────────────────╯*/
    // PDF, EPS and PostScript thumbnails stay off: KDE's gsthumbnail and
    // GNOME's document thumbnailers make them.  Nothing on KDE previews
    // `application/illustrator`, which shared-mime-info gives `.ai` by name
    // whether the file is a PDF or PostScript inside.  Slerp is for
    // graphics, not documents, so it is offered in "Open with" for a
    // document format only where the desktop has no viewer of its own:
    // never for PDF; for PostScript only on Windows and macOS (Preview
    // stopped opening it in macOS 13).  The PDF renderer is kept for
    // Illustrator files, which are PDFs inside.
    Format {
        id: "pdf",
        name: "PDF document",
        category: Drawing,
        mime: &[sys("application/pdf", &["pdf"])],
        thumbnails: Thumbs::Off,
        viewers: KDE | GNOME | WINDOWS | MACOS,
        others: KDE | GNOME | MACOS,
        ..BASE
    },
    Format {
        id: "illustrator",
        name: "Adobe Illustrator drawing",
        category: Drawing,
        mime: &[sys("application/illustrator", &["ai"])],
        mac: Mac::System(&["com.adobe.illustrator.ai-image"]),
        others: MACOS,
        ..BASE
    },
    Format {
        id: "eps",
        name: "Encapsulated PostScript (EPS)",
        category: Drawing,
        mime: &[sys("image/x-eps", &["eps", "epsi", "epsf"])],
        thumbnails: Thumbs::Off,
        mac: Mac::System(&["com.adobe.encapsulated-postscript"]),
        others: KDE | GNOME,
        ..BASE
    },
    Format {
        id: "postscript",
        name: "PostScript document",
        category: Drawing,
        mime: &[sys("application/postscript", &["ps"])],
        thumbnails: Thumbs::Off,
        viewers: KDE | GNOME,
        mac: Mac::System(&["com.adobe.postscript"]),
        others: KDE | GNOME,
        ..BASE
    },
    Format {
        id: "coreldraw",
        name: "CorelDRAW drawing",
        category: Preview,
        mime: &[
            sys("application/vnd.corel-draw", &["cdr"]),
            own("application/x-corel-draw-template", &["cdt"]),
            own("image/x-corel-cmx", &["cmx"]),
        ],
        // Kept out of "Open with", so Slerp doesn't become the default for
        // CorelDRAW files where nothing else opens them.
        open_with: false,
        // macOS calls .cdr a CD master disk image.
        mac: Mac::Import(&[uti_for("com.corel.coreldraw.template", &["cdt"]), uti_for("com.corel.cmx", &["cmx"])]),
        ..BASE
    },
    Format {
        id: "visio",
        name: "Visio drawing",
        category: Preview,
        mime: &[
            sys("application/vnd.visio", &["vsd", "vss", "vst"]),
            sys("application/vnd.ms-visio.drawing.main+xml", &["vsdx"]),
            sys("application/vnd.ms-visio.drawing.macroEnabled.main+xml", &["vsdm"]),
            sys("application/vnd.ms-visio.template.main+xml", &["vstx"]),
            sys("application/vnd.ms-visio.stencil.main+xml", &["vssx"]),
        ],
        open_with: false,
        mac: Mac::Import(&[uti("com.microsoft.visio")]),
        others: WINDOWS,
        ..BASE
    },
    /*┌─────────────────╮
    | Science, film   |
    └─────────────────╯*/
    Format {
        id: "fits",
        name: "FITS astronomy image",
        category: Science,
        mime: &[extends("application/fits", &["fits", "fit", "fts", "fits.fz", "fit.fz", "fts.fz"])],
        qt: Qt::Still,
        mac: Mac::Import(&[Uti { conforms: &["public.image", "public.data"], ..uti("gov.nasa.gsfc.fits") }]),
        ..BASE
    },
    Format {
        id: "xisf",
        name: "PixInsight image (XISF)",
        category: Science,
        mime: &[own("image/x-xisf", &["xisf"])],
        qt: Qt::Still,
        mac: Mac::Import(&[uti("com.pixinsight.xisf")]),
        ..BASE
    },
    Format {
        id: "dpx",
        name: "DPX film scan",
        category: Image,
        mime: &[extends("image/dpx", &["dpx"])],
        qt: Qt::Still,
        mac: Mac::Import(&[uti("org.smpte.dpx")]),
        ..BASE
    },
    Format {
        id: "cineon",
        name: "Cineon film scan",
        category: Image,
        mime: &[own("image/x-cineon", &["cin", "cineon"])],
        qt: Qt::Still,
        mac: Mac::Import(&[uti("com.kodak.cineon")]),
        ..BASE
    },
    Format {
        id: "pam",
        name: "Netpbm PAM image",
        category: Image,
        mime: &[own("image/x-portable-arbitrarymap", &["pam"])],
        qt: Qt::Still,
        mac: Mac::Import(&[uti("net.sourceforge.netpbm.pam")]),
        ..BASE
    },
    Format {
        id: "farbfeld",
        name: "farbfeld image",
        category: Image,
        mime: &[own("image/x-farbfeld", &["ff"])],
        qt: Qt::Still,
        mac: Mac::Import(&[uti("org.suckless.farbfeld")]),
        ..BASE
    },
    Format {
        id: "jxr",
        name: "JPEG XR image",
        category: Image,
        mime: &[sys("image/jxr", &["jxr", "wdp", "hdp"])],
        mac: Mac::Import(&[uti("com.microsoft.hdphoto")]),
        others: WINDOWS,
        ..BASE
    },
    /*┌─────────────────╮
    | Volumes         |
    └─────────────────╯*/
    Format {
        id: "numpy",
        name: "NumPy array",
        category: Science,
        mime: &[own("application/x-numpy-data", &["npy"]), own("application/x-numpy-archive", &["npz"])],
        mac: Mac::Import(&[uti_for("org.numpy.npy", &["npy"]), uti_for("org.numpy.npz", &["npz"])]),
        ..BASE
    },
    Format {
        id: "nifti",
        name: "NIfTI medical volume",
        category: Medical,
        mime: &[own("application/x-nifti", &["nii", "nii.gz"])],
        mac: Mac::System(&["gov.nih.nifti-1"]),
        ..BASE
    },
    Format {
        id: "analyze",
        name: "Analyze or NIfTI header (.hdr beside an .img)",
        category: Medical,
        mime: &[own("application/x-analyze-header", &[])],
        // Radiance's .hdr, told apart by content.
        by_content: &["hdr"],
        ..BASE
    },
    Format {
        id: "metaimage",
        name: "MetaImage volume",
        category: Medical,
        mime: &[own("application/x-metaimage", &["mha", "mhd"])],
        mac: Mac::Import(&[uti("org.itk.metaimage")]),
        ..BASE
    },
    Format {
        id: "nrrd",
        name: "NRRD volume",
        category: Medical,
        mime: &[own("application/x-nrrd", &["nrrd", "nhdr"])],
        mac: Mac::Import(&[uti("net.sourceforge.teem.nrrd")]),
        ..BASE
    },
    Format {
        id: "ome-tiff",
        name: "OME-TIFF microscopy image",
        category: Science,
        mime: &[own("image/x-ome-tiff", &["ome.tif", "ome.tiff", "ome.btf", "ome.tf2", "ome.tf8"])],
        ..BASE
    },
    Format {
        id: "bigtiff",
        name: "BigTIFF image",
        category: Science,
        mime: &[own("image/x-bigtiff", &["btf", "tf2", "tf8"])],
        mac: Mac::Import(&[Uti { conforms: &["public.image", "public.data"], ..uti("com.awaresystems.bigtiff") }]),
        ..BASE
    },
    Format {
        id: "aperio-svs",
        name: "Aperio whole-slide image (SVS)",
        category: Science,
        mime: &[own("image/x-aperio-svs", &["svs"])],
        mac: Mac::Import(&[Uti { conforms: &["public.image", "public.data"], ..uti("com.leica.aperio.svs") }]),
        ..BASE
    },
    Format {
        id: "ndpi",
        name: "Hamamatsu whole-slide image (NDPI)",
        category: Science,
        mime: &[own("image/x-hamamatsu-ndpi", &["ndpi"])],
        mac: Mac::Import(&[Uti { conforms: &["public.image", "public.data"], ..uti("com.hamamatsu.ndpi") }]),
        ..BASE
    },
    Format {
        id: "leica-scn",
        name: "Leica whole-slide image (SCN)",
        category: Science,
        // macOS gives .scn to SceneKit.
        mime: &[own("image/x-leica-scn", &["scn"])],
        ..BASE
    },
    Format {
        id: "hdf5",
        name: "HDF5 data",
        category: Science,
        // Not .h4 and .hdf4: HDF4 is a different format.
        mime: &[sys("application/x-hdf", &["h5", "hdf5", "hdf"])],
        by_content: &["he5"],
        mac: Mac::Import(&[uti("org.hdfgroup.hdf5")]),
        ..BASE
    },
    Format {
        id: "netcdf",
        name: "NetCDF data",
        category: Science,
        // Not .cdf, which is NASA's Common Data Format as often as NetCDF.
        mime: &[extends("application/x-netcdf", &["nc"])],
        mac: Mac::Import(&[uti("edu.ucar.unidata.netcdf")]),
        ..BASE
    },
    Format {
        id: "matlab",
        name: "MATLAB v7.3 data",
        category: Science,
        mime: &[own("application/x-matlab-data", &[])],
        // .mat is Unity's materials too.
        by_content: &["mat"],
        ..BASE
    },
    /*┌─────────────────╮
    | Archives        |
    └─────────────────╯*/
    Format {
        id: "quake-pak",
        name: "Quake PAK archive",
        category: Archive,
        mime: &[own("application/x-quake-pak", &["pak"])],
        mac: Mac::Import(&[uti("com.idsoftware.pak")]),
        ..BASE
    },
    Format {
        id: "idtech4-pk4",
        name: "id Tech 4 archive (PK4)",
        category: Archive,
        mime: &[own("application/x-idtech4-pk4", &["pk4"])],
        mac: Mac::Import(&[Uti { conforms: &["public.zip-archive", "public.data"], ..uti("com.idsoftware.pk4") }]),
        ..BASE
    },
    Format {
        id: "godot-pck",
        name: "Godot package (PCK)",
        category: Archive,
        mime: &[own("application/x-godot-pck", &[])],
        // Wwise audio packages are .pck too.
        by_content: &["pck"],
        ..BASE
    },
    Format {
        id: "valve-vpk",
        name: "Valve package (VPK)",
        category: Archive,
        mime: &[own("application/x-valve-vpk", &["vpk"])],
        mac: Mac::Import(&[uti("com.valvesoftware.vpk")]),
        ..BASE
    },
    Format {
        id: "build-grp",
        name: "Build engine group file (GRP)",
        category: Archive,
        mime: &[own("application/x-build-grp", &["grp"])],
        mac: Mac::Import(&[uti("com.3drealms.build.grp")]),
        ..BASE
    },
    /*┌─────────────────╮
    | Pictures        |
    └─────────────────╯*/
    Format {
        id: "gif",
        name: "GIF image",
        mime: &[sys("image/gif", &["gif"])],
        mac: Mac::System(&["com.compuserve.gif"]),
        ..PICTURE
    },
    Format { id: "apng", name: "Animated PNG", mime: &[extends("image/apng", &["apng"])], ..PICTURE },
    Format {
        id: "webp",
        name: "WebP image",
        mime: &[sys("image/webp", &["webp"])],
        mac: Mac::System(&["org.webmproject.webp"]),
        ..PICTURE
    },
    Format {
        id: "tiff",
        name: "TIFF image",
        mime: &[sys("image/tiff", &["tif", "tiff"])],
        mac: Mac::System(&["public.tiff"]),
        ..PICTURE
    },
    Format {
        id: "ico",
        name: "Windows icon or cursor",
        mime: &[sys("image/vnd.microsoft.icon", &["ico"]), sys("image/x-win-bitmap", &["cur"])],
        mac: Mac::System(&["com.microsoft.ico"]),
        ..PICTURE
    },
    Format {
        id: "pnm",
        name: "Netpbm image (PBM, PGM, PPM, PNM)",
        mime: &[
            sys("image/x-portable-bitmap", &["pbm"]),
            sys("image/x-portable-graymap", &["pgm"]),
            sys("image/x-portable-pixmap", &["ppm"]),
            sys("image/x-portable-anymap", &["pnm"]),
        ],
        mac: Mac::System(&["public.pbm"]),
        ..PICTURE
    },
    Format {
        id: "jpeg2000",
        name: "JPEG 2000 image",
        mime: &[
            sys("image/jp2", &["jp2", "jpg2"]),
            sys("image/jpx", &["jpx", "jpf"]),
            sys("image/x-jp2-codestream", &["j2c", "jpc", "j2k"]),
        ],
        mac: Mac::System(&["public.jpeg-2000"]),
        ..PICTURE
    },
    Format {
        id: "jxl",
        name: "JPEG XL image",
        mime: &[sys("image/jxl", &["jxl"])],
        mac: Mac::System(&["public.jpeg-xl"]),
        ..PICTURE
    },
    Format {
        id: "avif",
        name: "AVIF image",
        mime: &[sys("image/avif", &["avif", "avifs"])],
        mac: Mac::System(&["public.avif"]),
        ..PICTURE
    },
    Format {
        id: "heif",
        name: "HEIF or HEIC image",
        mime: &[sys("image/heif", &["heic", "heif", "hif"])],
        // HEIF image sequences, which shared-mime-info doesn't name.
        by_content: &["heics"],
        mac: Mac::System(&["public.heic", "public.heif"]),
        ..PICTURE
    },
    Format { id: "png", name: "PNG image", mime: &[sys("image/png", &["png"])], open_with: false, ..PICTURE },
    Format {
        id: "jpeg",
        name: "JPEG image",
        mime: &[sys("image/jpeg", &["jpg", "jpeg", "jpe"])],
        open_with: false,
        ..PICTURE
    },
    Format { id: "bmp", name: "BMP image", mime: &[sys("image/bmp", &["bmp", "dib"])], open_with: false, ..PICTURE },
    Format { id: "tga", name: "TGA image", mime: &[sys("image/x-tga", &["tga"])], open_with: false, ..PICTURE },
    Format {
        id: "pcx",
        name: "PCX image",
        mime: &[sys("image/vnd.zbrush.pcx", &["pcx"])],
        open_with: false,
        others: KDE | WINDOWS,
        ..PICTURE
    },
    /*┌─────────────────────╮
    | Phase 23: 2D images |
    └─────────────────────╯*/
    // KDE previews every one of these (Qt's and kimageformats' readers, the
    // SVG and Krita/OpenRaster thumbnailers), so Slerp's thumbnails are off
    // unless the user turns them on; the viewer opens them all.
    Format {
        id: "svg",
        name: "SVG drawing",
        category: Drawing,
        mime: &[sys("image/svg+xml", &["svg"]), sys("image/svg+xml-compressed", &["svgz", "svg.gz"])],
        mac: Mac::System(&["public.svg-image"]),
        others: KDE | GNOME | MACOS,
        ..PICTURE
    },
    Format {
        id: "psd",
        name: "Photoshop document",
        // Photoshop's large-document format shares the magic; shared-mime-info
        // only lacks its extension.
        mime: &[extends("image/vnd.adobe.photoshop", &["psd", "psb"])],
        mac: Mac::System(&["com.adobe.photoshop-image"]),
        others: KDE | MACOS,
        ..PICTURE
    },
    Format {
        id: "xcf",
        name: "GIMP image",
        mime: &[sys("image/x-xcf", &["xcf"]), sys("image/x-compressed-xcf", &["xcf.gz", "xcf.bz2"])],
        mac: Mac::Import(&[uti_for("org.gimp.xcf", &["xcf"])]),
        others: KDE,
        ..PICTURE
    },
    Format {
        id: "krita",
        name: "Krita document",
        mime: &[sys("application/x-krita", &["kra", "krz"])],
        mac: Mac::Import(&[uti("org.kde.krita")]),
        others: KDE,
        ..PICTURE
    },
    Format {
        id: "openraster",
        name: "OpenRaster image",
        mime: &[sys("image/openraster", &["ora"])],
        mac: Mac::Import(&[uti("org.openraster.ora")]),
        others: KDE,
        ..PICTURE
    },
    Format {
        id: "openexr",
        name: "OpenEXR image",
        mime: &[sys("image/x-exr", &["exr"])],
        mac: Mac::System(&["com.ilm.openexr-image"]),
        others: KDE | GNOME | MACOS,
        ..PICTURE
    },
    Format {
        id: "qoi",
        name: "QOI image",
        mime: &[sys("image/qoi", &["qoi"])],
        mac: Mac::Import(&[uti("org.qoiformat.qoi")]),
        others: KDE | GNOME,
        ..PICTURE
    },
    Format {
        id: "icns",
        name: "Apple icon",
        mime: &[sys("image/x-icns", &["icns"])],
        mac: Mac::System(&["com.apple.icns"]),
        others: KDE | MACOS,
        ..PICTURE
    },
    Format {
        id: "sgi",
        name: "SGI image",
        // shared-mime-info names `.sgi` (without magic) and `.rgb` as its own
        // type.  SGI's `.rgba`, `.bw`, `.int` and `.inta` are found by the
        // magic slerp.xml adds, never by name: raw RGBA dumps, bigWig
        // genome tracks and others share those extensions.
        mime: &[extends("image/x-sgi", &["sgi"]), sys("image/x-rgb", &["rgb"])],
        by_content: &["rgba", "bw", "int", "inta"],
        mac: Mac::System(&["com.sgi.sgi-image"]),
        others: KDE | MACOS,
        ..PICTURE
    },
    Format {
        id: "xpm",
        name: "X pixmap (XPM)",
        mime: &[sys("image/x-xpixmap", &["xpm"])],
        mac: Mac::Import(&[uti("org.x.xpm")]),
        others: KDE,
        ..PICTURE
    },
    Format {
        id: "xbm",
        name: "X bitmap (XBM)",
        mime: &[sys("image/x-xbitmap", &["xbm"])],
        mac: Mac::System(&["public.xbitmap-image"]),
        others: KDE,
        ..PICTURE
    },
    /*┌─────────────────╮
    | Stored previews  |
    | (phase 23)       |
    └─────────────────╯*/
    // CAD and creative-app files Slerp shows by the picture their program
    // saved.  None is offered in "Open with": Slerp must never become their
    // default app.  Extensions other files share (`.par`, `.asm`, `.psp`,
    // `.tub`, `.pxd`) are claimed by content alone, through magic in
    // slerp.xml: a glob would make every assembler source or Cython
    // declaration file a CAD file.
    Format {
        id: "dwg",
        name: "AutoCAD drawing",
        category: Preview,
        mime: &[sys("image/vnd.dwg", &["dwg"])],
        open_with: false,
        mac: Mac::Import(&[uti("com.autodesk.dwg")]),
        ..BASE
    },
    Format {
        id: "dwf",
        name: "Autodesk DWF or DWFx file",
        category: Preview,
        mime: &[own("model/vnd.dwf", &["dwf"]), own("model/vnd.dwfx+xps", &["dwfx"])],
        open_with: false,
        mac: Mac::Import(&[uti_for("com.autodesk.dwf", &["dwf"]), uti_for("com.autodesk.dwfx", &["dwfx"])]),
        ..BASE
    },
    Format {
        id: "solidworks",
        name: "SolidWorks part, assembly or drawing",
        category: Preview,
        mime: &[
            own("application/x-solidworks-part", &["sldprt"]),
            own("application/x-solidworks-assembly", &["sldasm"]),
            own("application/x-solidworks-drawing", &["slddrw"]),
        ],
        open_with: false,
        mac: Mac::Import(&[
            uti_for("com.solidworks.sldprt", &["sldprt"]),
            uti_for("com.solidworks.sldasm", &["sldasm"]),
            uti_for("com.solidworks.slddrw", &["slddrw"]),
        ]),
        ..BASE
    },
    Format {
        id: "inventor",
        name: "Autodesk Inventor file",
        category: Preview,
        mime: &[
            own("application/x-autodesk-inventor-part", &["ipt"]),
            own("application/x-autodesk-inventor-assembly", &["iam"]),
            own("application/x-autodesk-inventor-drawing", &["idw"]),
            own("application/x-autodesk-inventor-presentation", &["ipn"]),
        ],
        open_with: false,
        mac: Mac::Import(&[
            uti_for("com.autodesk.inventor.ipt", &["ipt"]),
            uti_for("com.autodesk.inventor.iam", &["iam"]),
            uti_for("com.autodesk.inventor.idw", &["idw"]),
            uti_for("com.autodesk.inventor.ipn", &["ipn"]),
        ]),
        ..BASE
    },
    Format {
        id: "revit",
        name: "Autodesk Revit project or family",
        category: Preview,
        mime: &[
            own("application/x-autodesk-revit-project", &["rvt"]),
            own("application/x-autodesk-revit-family", &["rfa"]),
            own("application/x-autodesk-revit-template", &["rte"]),
        ],
        open_with: false,
        mac: Mac::Import(&[
            uti_for("com.autodesk.revit.rvt", &["rvt"]),
            uti_for("com.autodesk.revit.rfa", &["rfa"]),
            uti_for("com.autodesk.revit.rte", &["rte"]),
        ]),
        ..BASE
    },
    Format {
        id: "solid-edge",
        name: "Solid Edge part, assembly or draft",
        category: Preview,
        mime: &[
            own("application/x-solidedge-part", &[]),
            own("application/x-solidedge-sheetmetal", &["psm"]),
            own("application/x-solidedge-assembly", &[]),
            own("application/x-solidedge-draft", &["dft"]),
        ],
        // Parity files and parameter lists are .par too, assembler source
        // .asm: those two are known by the documents' class ids.
        by_content: &["par", "asm"],
        open_with: false,
        mac: Mac::Import(&[
            uti_for("com.siemens.solidedge.psm", &["psm"]),
            uti_for("com.siemens.solidedge.dft", &["dft"]),
        ]),
        ..BASE
    },
    Format {
        id: "fusion360",
        name: "Autodesk Fusion design",
        category: Preview,
        mime: &[own("application/x-autodesk-fusion360", &["f3d"])],
        open_with: false,
        mac: Mac::Import(&[uti("com.autodesk.fusion360.f3d")]),
        ..BASE
    },
    Format {
        id: "iwork",
        name: "Apple Keynote, Pages or Numbers document",
        category: Preview,
        mime: &[
            sys("application/vnd.apple.keynote", &["key"]),
            sys("application/vnd.apple.pages", &["pages"]),
            sys("application/vnd.apple.numbers", &["numbers"]),
        ],
        open_with: false,
        mac: Mac::System(&["com.apple.iwork.keynote.key", "com.apple.iwork.pages.pages", "com.apple.iwork.numbers.numbers"]),
        others: MACOS,
        ..BASE
    },
    Format {
        id: "paintshop-pro",
        name: "Paint Shop Pro image, frame or tube",
        category: Preview,
        mime: &[
            own("image/x-paintshoppro", &["pspimage"]),
            own("image/x-paintshoppro-frame", &["pspframe"]),
            own("image/x-paintshoppro-tube", &["psptube"]),
        ],
        // Paint Shop Pro 5 to 7 wrote .psp, .tub and (frames) .pfr, which
        // other programs use too; the 32-byte signature tells.
        by_content: &["psp", "tub", "pfr"],
        open_with: false,
        mac: Mac::Import(&[
            uti_for("com.corel.paintshoppro.pspimage", &["pspimage"]),
            uti_for("com.corel.paintshoppro.pspframe", &["pspframe"]),
            uti_for("com.corel.paintshoppro.psptube", &["psptube"]),
        ]),
        ..BASE
    },
    Format {
        id: "pixelmator-pro",
        name: "Pixelmator Pro document",
        category: Preview,
        mime: &[own("image/x-pixelmator-pro", &[])],
        // Cython declaration files are .pxd too.
        by_content: &["pxd"],
        open_with: false,
        ..BASE
    },
    Format {
        id: "painttool-sai",
        name: "PaintTool SAI document",
        category: Preview,
        mime: &[own("image/x-sai", &["sai"]), own("image/x-sai2", &["sai2"])],
        open_with: false,
        mac: Mac::Import(&[uti_for("jp.co.systemax.sai", &["sai"]), uti_for("jp.co.systemax.sai2", &["sai2"])]),
        ..BASE
    },
    Format {
        id: "pyxel-edit",
        name: "Pyxel Edit document",
        category: Sprite,
        mime: &[own("image/x-pyxel", &["pyxel"])],
        open_with: false,
        mac: Mac::Import(&[uti("com.pyxeledit.pyxel")]),
        ..BASE
    },
    Format {
        id: "piskel",
        name: "Piskel sprite",
        category: Sprite,
        mime: &[own("image/x-piskel", &["piskel"])],
        open_with: false,
        mac: Mac::Import(&[uti("com.piskelapp.piskel")]),
        ..BASE
    },
    Format {
        id: "unity-package",
        name: "Unity package",
        category: Archive,
        mime: &[own("application/x-unitypackage", &["unitypackage"])],
        open_with: false,
        mac: Mac::Import(&[uti("com.unity3d.unitypackage")]),
        ..BASE
    },
];
