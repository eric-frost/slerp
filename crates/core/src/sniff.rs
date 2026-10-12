//! Recognising formats by content, with the extension only as a tiebreak.

use slerp_materials::MaterialFormat;
use slerp_models::ModelFormat;
use slerp_quake::QuakeFormat;
use slerp_textures::Format as TextureFormat;
use std::io::Read;
use std::path::Path;

/// Which loader reads a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Texture(TextureFormat),
    Gltf,
    Model(ModelFormat),
    Quake(QuakeFormat),
    Material(MaterialFormat),
    /// A MetaHuman DNA file (head or body rig).
    Dna,
    /// An Aseprite sprite or a Tiled map.
    Sprite(slerp_sprites::SpriteFormat),
    /// A colour lookup table.
    Lut(slerp_lut::LutFormat),
    /// Gaussian splats: 3DGS PLY, compressed PLY, `.splat` or SPZ.
    Splat(slerp_splats::SplatFormat),
    /// A file shown by the picture its application saved in it, whose
    /// content Slerp doesn't read: 3D and CAD programs' (3ds Max, Cinema 4D,
    /// AutoCAD, SolidWorks, Revit and others) and creative apps' (Procreate,
    /// iWork, Paint Shop Pro, PaintTool SAI and others).
    Preview(slerp_previews::PreviewFormat),
    /// A game engine's scene: Godot `.tscn`/`.scn`/`.escn` and mesh
    /// resources, Unity `.unity`/`.prefab`.
    Engine(slerp_engines::EngineFormat),
    /// A Blender `.blend` file (plain, gzip or Zstandard compressed).
    Blend,
    /// A colour palette: Adobe swatches and colour tables, GIMP, Krita,
    /// JASC and RIFF palettes, hex lists, Procreate swatches.
    Palette(slerp_previews::palette::PaletteFormat),
    /// A DICOM medical image, or a DICOMDIR (the index of a CD of them).
    Dicom,
    /// G-code (text or Prusa's binary) or a GPS track (GPX, TCX, FIT, KML,
    /// KMZ, GeoJSON).
    Path(slerp_paths::PathFormat),
    /// A CAD drawing or model: AutoCAD DXF (text or binary) or Rhino `.3dm`.
    Cad(slerp_cad::CadFormat),
    /// A picture from a retro computer (Atari ST, C64, ZX Spectrum, CPC, MSX,
    /// Apple II, Atari 8-bit, ANSI art, PICO-8 and TIC-80 cartridges, Amiga
    /// IFF).
    Retro(slerp_retro::RetroFormat),
    /// A game image other tools skip: XNB, BLP, PVR, PKM or Build ART tiles.
    GameImage(slerp_gameimages::GameImageFormat),
    /// A Windows or Macintosh metafile (WMF, EMF, their gzip forms, PICT),
    /// replayed onto a canvas.
    Metafile(slerp_metafile::MetafileFormat),
    /// A scientific array, medical volume or microscopy image: NumPy, NIfTI,
    /// MetaImage, NRRD, TIFF stacks and slides, NetCDF, HDF5.
    Volume(slerp_volumes::VolumeFormat),
    /// A game archive no other loader reads: a Quake or Quake 2 PAK, a Godot
    /// PCK, a Valve VPK or a Build GRP.  Shown by a contact sheet of the
    /// pictures inside, and browsed through `contents`.
    Archive(slerp_archive::ArchiveKind),
    /// An ordinary picture the texture crate does not read: GIF, animated
    /// PNG and WebP, icons, PNM, TIFF photographs, JPEG 2000, JPEG XL, AVIF
    /// and HEIC.
    Image(slerp_images::ImageFormat),
    /// A page-description document: PDF, Adobe Illustrator, EPS or
    /// PostScript.
    Document(slerp_documents::DocumentFormat),
    /// A Minecraft schematic: Sponge `.schem`, Litematica, a structure
    /// `.nbt` or MCEdit `.schematic`, drawn as blocks.
    Minecraft(slerp_minecraft::MinecraftFormat),
    /// A plain Draco file (`.drc`): one compressed mesh or point cloud.
    Draco,
    /// A skinned model or animation from games and animation tools:
    /// MikuMikuDance PMX, PMD and VMD, Doom 3 MD5, IQM and IQE, Source SMD,
    /// Unreal PSK and PSA.
    Rigged(slerp_rigged::RiggedFormat),
    /// A layered paint document: Photoshop PSD and PSB, GIMP XCF, OpenRaster
    /// ORA, Krita KRA.
    Paint(slerp_paint::PaintFormat),
    /// An SVG or SVGZ drawing.
    Svg,
    /// A SketchUp model (`.skp`): its geometry in the viewer, the picture
    /// SketchUp saved in it for thumbnails.
    SketchUp,
    /// A Roblox place or model, binary (`.rbxl`, `.rbxm`) or XML (`.rbxlx`,
    /// `.rbxmx`, and `.rbxl` before 2013).
    Roblox(slerp_roblox::RobloxFormat),
    /// A Maya scene: Maya ASCII (`.ma`) or Maya binary (`.mb`).  Neither
    /// carries a picture of itself, so thumbnails render it.
    Maya(slerp_maya::MayaFormat),
    /// A 3ds Max scene (`.max`): its geometry in the viewer, the picture
    /// Max saved in it for thumbnails.
    Max,
}

impl Kind {
    /// True when thumbnails of this kind usually need the GPU, so the
    /// thumbnailer should ask the helper rather than load the file itself.
    /// Materials count, because most render as a ball.
    pub fn usually_3d(self) -> bool {
        match self {
            Kind::Texture(_)
            | Kind::Material(MaterialFormat::Sbsar)
            | Kind::Sprite(_)
            | Kind::Lut(_)
            | Kind::Preview(_)
            | Kind::Palette(_)
            | Kind::Retro(_)
            | Kind::GameImage(_)
            | Kind::Metafile(_)
            | Kind::Volume(_)
            | Kind::Archive(_)
            | Kind::Image(_)
            | Kind::Document(_)
            | Kind::Paint(_)
            | Kind::Svg
            | Kind::SketchUp
            | Kind::Max
            | Kind::Dicom => false,
            // Animation files have nothing to draw.
            Kind::Rigged(f) => !f.is_animation(),
            // Tracks are drawn flat; a G-code file without a picture of its
            // own is rendered.
            Kind::Path(f) => f.is_toolpath(),
            // Flat DXF drawings are drawn on the CPU, quicker than asking
            // the helper; the rare 3D one renders in the process.
            Kind::Cad(f) => f.usually_3d(),
            Kind::Quake(f) => !matches!(
                f,
                QuakeFormat::Wad2
                    | QuakeFormat::Wad3
                    | QuakeFormat::DoomWad
                    | QuakeFormat::DoomArchive
                    | QuakeFormat::UnrealAsset
            ),
            _ => true,
        }
    }
}

/// Bytes read from the start of a file for sniffing.  Collada and MaterialX
/// put their root element after an XML declaration and comments, so this is
/// generous.
pub const SNIFF_BYTES: usize = 4096;

/// Reads the start of `path` and recognises its format.
pub fn sniff_file(path: &Path) -> std::io::Result<Option<Kind>> {
    let mut head = Vec::with_capacity(SNIFF_BYTES);
    std::fs::File::open(path)?.take(SNIFF_BYTES as u64).read_to_end(&mut head)?;
    Ok(sniff(path, &head))
}

/// Recognises a format from the file's first bytes and name.  Formats with
/// strong magic numbers are tried first, so a misnamed file still loads.
pub fn sniff(path: &Path, head: &[u8]) -> Option<Kind> {
    // DICOM first: `DICM` after a 128-byte preamble that may hold anything
    // (dual-personality DICOM-TIFF files put a TIFF header there).
    if head.get(128..132) == Some(b"DICM".as_slice()) {
        return Some(Kind::Dicom);
    }
    if slerp_gltf::sniff(path, head) {
        return Some(Kind::Gltf);
    }
    if slerp_gltf::sniff_drc(head) {
        return Some(Kind::Draco);
    }
    // `<roblox!` and a signature, or a `<roblox` root element: before the
    // other XML formats, whatever the name says.
    if let Some(f) = slerp_roblox::sniff(path, head) {
        return Some(Kind::Roblox(f));
    }
    // "//Maya ASCII" or an IFF group of type "Maya": by content only,
    // since Mathematica and others use the same extensions.
    if let Some(f) = slerp_maya::sniff(head) {
        return Some(Kind::Maya(f));
    }
    // Every one has a signature, the text ones (MD5, IQE, SMD) a first
    // line of their own; before the weak guesses below.
    if let Some(f) = slerp_rigged::sniff(path, head) {
        return Some(Kind::Rigged(f));
    }
    if let Some(f) = slerp_quake::sniff(path, head) {
        return Some(Kind::Quake(f));
    }
    if let Some(k) = archive_kind(head) {
        return Some(Kind::Archive(k));
    }
    // Early, before the weak picture guesses: schematics are named by
    // their extension and confirmed as NBT (a structure `.nbt` by its keys).
    if let Some(f) = slerp_minecraft::sniff(path, head) {
        return Some(Kind::Minecraft(f));
    }
    // Retro dumps have no magic, so they are known by extension and exact
    // size (or by IFF's magic); game images by their magic.  Both go before
    // the heuristics for headerless texture formats below, which would take
    // some of them for TGA or PCX.
    if let Some(f) = slerp_retro::sniff(path, head) {
        return Some(Kind::Retro(f));
    }
    if let Some(f) = slerp_gameimages::sniff(path, head) {
        return Some(Kind::GameImage(f));
    }
    // Before textures: Adobe colour tables have no magic, only their size,
    // and could pass for a headerless picture format.
    if let Some(f) = slerp_previews::palette::sniff(path, head) {
        return Some(Kind::Palette(f));
    }
    // Before textures too: a metafile's header would pass the weak TGA and
    // PCX guesses.
    if let Some(f) = slerp_metafile::sniff(path, head) {
        return Some(Kind::Metafile(f));
    }
    // PDF's `%PDF-`, PostScript's `%!` and the DOS EPS header: strong
    // signatures, and text the guesses below might take for something else.
    if let Some(f) = slerp_documents::sniff(path, head) {
        return Some(Kind::Document(f));
    }
    // Ordinary pictures the texture crate leaves alone, and animated PNG and
    // WebP, which it would take for stills.  Before the volumes: a TIFF
    // photograph is a picture, a stack of microscope slices a volume.  The
    // volumes keep the TIFFs that only look like photographs with thumbnails:
    // a slide's label and macro pages are flagged as reduced copies.
    if let Some(f) = slerp_images::sniff(path, head)
        && !(f == slerp_images::ImageFormat::Tiff && slerp_volumes::tiff::is_slide_or_hyperstack(path))
    {
        return Some(Kind::Image(f));
    }
    // Layered paint documents by their magic (`8BPS`, `gimp xcf`, a zip
    // whose first entry names OpenRaster or Krita), before the creative-app
    // previews, which also read zips.  SVG by its root element, before the
    // text formats further down.
    if let Some(f) = slerp_paint::sniff(path, head) {
        return Some(Kind::Paint(f));
    }
    if slerp_svg::is_svg(path, head) {
        return Some(Kind::Svg);
    }
    // Arrays and volumes by their signatures (NumPy's, NRRD's, HDF5's, TIFF's,
    // NetCDF's, NIfTI's headers); before the textures' weak guesses.
    if let Some(f) = slerp_volumes::sniff(path, head) {
        return Some(Kind::Volume(f));
    }
    if let Some(f) = slerp_textures::sniff(head) {
        // Ordinary images belong to other thumbnailers; only claim them when
        // nothing else does, which the MIME list already ensures.
        return Some(Kind::Texture(f));
    }
    // SketchUp models before the stored previews, which read their
    // pictures: the viewer shows the model itself.
    if slerp_sketchup::sniff(head) {
        return Some(Kind::SketchUp);
    }
    if slerp_max::sniff(path, head) {
        return Some(Kind::Max);
    }
    if let Some(f) = slerp_previews::sniff(path, head) {
        return Some(Kind::Preview(f));
    }
    // Both start with their own signature: DXF's sentinel or its first
    // `SECTION`, and Rhino's "3D Geometry File Format".
    if let Some(f) = slerp_cad::sniff(path, head) {
        return Some(Kind::Cad(f));
    }
    if slerp_blend::sniff(path, head) {
        return Some(Kind::Blend);
    }
    // Before the mesh loader, which would take SuperSplat's compressed
    // PLY for a mesh.
    if let Some(f) = slerp_splats::sniff(path, head) {
        return Some(Kind::Splat(f));
    }
    if let Some(f) = slerp_models::sniff(path, head) {
        return Some(Kind::Model(f));
    }
    // Before materials: Godot mesh and scene resources share `.tres` with
    // materials, which the engines crate leaves alone.
    if let Some(f) = slerp_engines::sniff(path, head) {
        return Some(Kind::Engine(f));
    }
    if let Some(f) = slerp_materials::sniff(path, head) {
        return Some(Kind::Material(f));
    }
    if slerp_dna::sniff(head) {
        return Some(Kind::Dna);
    }
    if let Some(f) = slerp_sprites::sniff(path, head) {
        return Some(Kind::Sprite(f));
    }
    if let Some(f) = slerp_lut::sniff(path, head) {
        return Some(Kind::Lut(f));
    }
    // Late: G-code is text known by its extension, and the XML and JSON
    // track formats by their root elements.
    if let Some(f) = slerp_paths::sniff(path, head) {
        return Some(Kind::Path(f));
    }
    // ANSI art that kept its SAUCE record under another name.
    if let Some(f) = slerp_retro::sniff_by_content(path, head) {
        return Some(Kind::Retro(f));
    }
    // DICOM data sets without the preamble (CD exports named `IM000001`),
    // recognised by a run of plausible header elements: last, so nothing
    // else is taken for one.
    if slerp_dicom::sniff(path, head) {
        return Some(Kind::Dicom);
    }
    None
}

/// The game archives that have no loader of their own, by their first bytes.
/// A `PACK` file is Quake's only when its directory fields fit (git pack
/// files also start with `PACK`), and `.pak` is never enough by itself:
/// other games reuse the extension.
fn archive_kind(head: &[u8]) -> Option<slerp_archive::ArchiveKind> {
    use slerp_archive::ArchiveKind;
    match slerp_archive::sniff(head)? {
        k @ (ArchiveKind::Pck | ArchiveKind::Vpk | ArchiveKind::Grp | ArchiveKind::UnityPackage) => Some(k),
        ArchiveKind::Pak => {
            let record = if head.starts_with(b"SPAK") { 128 } else { 64 };
            let dir = u32::from_le_bytes(head.get(4..8)?.try_into().ok()?);
            let len = u32::from_le_bytes(head.get(8..12)?.try_into().ok()?);
            (dir >= 12 && len > 0 && len % record == 0).then_some(ArchiveKind::Pak)
        }
        _ => None,
    }
}

/*┌─────────────────╮
| Formats         |
└─────────────────╯*/
//region Formats

impl Kind {
    /// The format table's entry for a file of this kind at `path` (the name
    /// tells apart the few the loaders share, such as Godot scenes and
    /// resources), or `None` for files Slerp reads only when opened by name
    /// (ZX Spectrum and Amstrad CPC screens) or by content alone.
    pub fn format(self, path: &Path) -> Option<&'static slerp_formats::Format> {
        let id = self.format_id(path)?;
        slerp_formats::by_id(id)
    }

    fn format_id(self, path: &Path) -> Option<&'static str> {
        use slerp_archive::ArchiveKind as A;
        use slerp_engines::EngineFormat as E;
        use slerp_images::ImageFormat as I;
        use slerp_paths::PathFormat as P;
        use slerp_previews::PreviewFormat as Pv;
        use slerp_previews::palette::PaletteFormat as Pal;
        use slerp_retro::RetroFormat as R;
        use slerp_volumes::VolumeFormat as V;
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_ascii_lowercase();
        let ext = |e: &str| name.ends_with(&format!(".{e}"));
        Some(match self {
            Kind::Texture(t) => match t {
                TextureFormat::Ktx2 => "ktx2",
                TextureFormat::Ktx1 => "ktx",
                TextureFormat::Dds => "dds",
                TextureFormat::Astc => "astc",
                TextureFormat::Hdr => "radiance-hdr",
                TextureFormat::Png => {
                    let named = slerp_formats::by_id("heightmap")?.mime[0]
                        .patterns
                        .iter()
                        .any(|p| slerp_formats::glob_matches(p, &name));
                    if named {
                        "heightmap"
                    } else if png_16bit_grey(path) {
                        "png-16bit"
                    } else {
                        "png"
                    }
                }
                TextureFormat::Jpeg => "jpeg",
                TextureFormat::Tga => "tga",
                TextureFormat::Bmp => "bmp",
                TextureFormat::Webp => "webp",
                TextureFormat::Pcx => "pcx",
                TextureFormat::Fits => "fits",
                TextureFormat::Xisf => "xisf",
                TextureFormat::Dpx => "dpx",
                TextureFormat::Cineon => "cineon",
                TextureFormat::Pam => "pam",
                TextureFormat::Farbfeld => "farbfeld",
                TextureFormat::Jxr => "jxr",
            },
            Kind::Gltf if ext("vrma") => "vrma",
            Kind::Gltf if ext("vrm") => "vrm",
            Kind::Gltf if ext("gltf") => "gltf",
            Kind::Gltf => "glb",
            Kind::Model(m) => match m {
                ModelFormat::Obj => "obj",
                ModelFormat::Stl => "stl",
                ModelFormat::Ply => "ply",
                ModelFormat::Fbx => "fbx",
                ModelFormat::Collada => "collada",
                ModelFormat::MagicaVoxel => "vox",
                ModelFormat::Off => "off",
                ModelFormat::ThreeMf => "3mf",
                ModelFormat::Max3ds => "3ds",
                ModelFormat::DirectX => "directx",
                ModelFormat::Vrml => "vrml",
                ModelFormat::X3d => "x3d",
                ModelFormat::Las => "las",
                ModelFormat::E57 => "e57",
                ModelFormat::Pcd => "pcd",
                ModelFormat::Pts => "pts",
                ModelFormat::Ptx => "ptx",
                ModelFormat::Blockbench => "blockbench",
            },
            Kind::Quake(q) => match q {
                QuakeFormat::Bsp => "quake-bsp",
                QuakeFormat::QuakeMdl | QuakeFormat::HalfLifeMdl => "quake-mdl",
                QuakeFormat::SourceMdl => "source-mdl",
                QuakeFormat::Md2 => "md2",
                QuakeFormat::Md3 => "md3",
                QuakeFormat::Wad2 | QuakeFormat::Wad3 => "quake-wad",
                QuakeFormat::DoomWad => "doom-wad",
                QuakeFormat::DoomArchive if ext("pk7") || ext("ipk7") => "doom-pk7",
                QuakeFormat::DoomArchive => "doom-pk3",
                QuakeFormat::UnrealAsset => "uasset",
                QuakeFormat::Map => "quake-map",
                QuakeFormat::Vmf => "valve-vmf",
                QuakeFormat::Sprite => "quake-sprite",
                QuakeFormat::Sprite2 => "quake2-sprite",
            },
            Kind::Material(m) => match m {
                MaterialFormat::MaterialX => "materialx",
                MaterialFormat::Usd => "usd",
                MaterialFormat::Usdz => "usdz",
                MaterialFormat::GodotResource => "godot-resource",
                MaterialFormat::Sbsar => "sbsar",
            },
            Kind::Dna => "metahuman-dna",
            Kind::Sprite(s) => match s {
                slerp_sprites::SpriteFormat::Aseprite => "aseprite",
                slerp_sprites::SpriteFormat::Tmx | slerp_sprites::SpriteFormat::Tmj => "tiled",
                slerp_sprites::SpriteFormat::Ldtk => "ldtk",
                slerp_sprites::SpriteFormat::Atlas if ext("atlas") => "libgdx-atlas",
                slerp_sprites::SpriteFormat::Atlas if ext("tpsheet") => "texturepacker",
                slerp_sprites::SpriteFormat::Atlas => return None,
                slerp_sprites::SpriteFormat::Piskel => "piskel",
                slerp_sprites::SpriteFormat::Pyxel => "pyxel-edit",
            },
            Kind::Lut(slerp_lut::LutFormat::Cube) => "cube-lut",
            Kind::Lut(slerp_lut::LutFormat::ThreeDl) => "3dl-lut",
            Kind::Splat(s) => match s {
                slerp_splats::SplatFormat::Ply | slerp_splats::SplatFormat::CompressedPly => "ply",
                slerp_splats::SplatFormat::Splat => "splat",
                slerp_splats::SplatFormat::Spz => "spz",
                slerp_splats::SplatFormat::Sog => "sog",
            },
            Kind::Preview(p) => match p {
                Pv::Max => "3dsmax",
                Pv::SketchUp => "sketchup",
                Pv::Cinema4d => "cinema4d",
                Pv::ZBrush => "zbrush",
                Pv::Procreate => "procreate",
                Pv::Affinity => "affinity",
                Pv::Sketch => "sketch",
                Pv::Figma => "figma",
                Pv::ClipStudio => "clip-studio",
                Pv::PaintNet => "paintnet",
                Pv::InDesign => "indesign",
                Pv::AdobeXd => "adobe-xd",
                Pv::FreeCad => "freecad",
                Pv::CorelDraw => "coreldraw",
                Pv::Visio => "visio",
                Pv::Dwg => "dwg",
                Pv::Dwf => "dwf",
                Pv::SolidWorks => "solidworks",
                Pv::Inventor => "inventor",
                Pv::Revit => "revit",
                Pv::SolidEdge => "solid-edge",
                Pv::Fusion => "fusion360",
                Pv::PaintShopPro => "paintshop-pro",
                Pv::Pixelmator => "pixelmator-pro",
                Pv::Sai => "painttool-sai",
                Pv::Iwork => "iwork",
            },
            Kind::Engine(E::GodotText | E::GodotBinary) if ext("tres") || ext("res") => "godot-resource",
            Kind::Engine(E::GodotText | E::GodotBinary) => "godot-scene",
            Kind::Engine(E::UnityYaml | E::UnityBinary) => "unity",
            Kind::Blend => "blend",
            Kind::Palette(p) => match p {
                Pal::Ase => "ase-palette",
                Pal::Aco => "aco",
                Pal::Act => "act",
                Pal::Gpl => "gpl",
                Pal::Kpl => "kpl",
                Pal::JascPal => "jasc-pal",
                Pal::RiffPal => "riff-pal",
                Pal::Hex => "hex-palette",
                Pal::ProcreateSwatches => "procreate-swatches",
            },
            Kind::Dicom => "dicom",
            Kind::Path(p) => match p {
                P::Gcode => "gcode",
                P::Bgcode => "bgcode",
                P::Gpx => "gpx",
                P::Tcx => "tcx",
                P::Fit => "fit",
                P::Kml | P::Kmz => "kml",
                P::GeoJson => "geojson",
            },
            Kind::Cad(slerp_cad::CadFormat::Dxf) => "dxf",
            Kind::Cad(slerp_cad::CadFormat::Rhino) => "rhino",
            Kind::Retro(r) => match r {
                R::Degas => "atari-degas",
                R::Neochrome => "atari-neochrome",
                R::TinyStuff => "atari-tiny",
                R::C64(k) => match k.name() {
                    "Koala Painter" => "c64-koala",
                    "Art Studio" => "c64-artstudio",
                    "Doodle" => "c64-doodle",
                    _ => "c64-hires",
                },
                R::Spectrum | R::Cpc => return None,
                R::Msx(_) => "msx",
                R::AppleHires => "apple2-hires",
                R::AppleDoubleHires => "apple2-double-hires",
                R::Atari8(_) => "atari8",
                R::Bbc => "bbc",
                R::Ansi => "ansi-art",
                R::Pico8 => "pico8",
                R::Tic80 => "tic80",
                R::Ilbm => "ilbm",
            },
            Kind::GameImage(g) => match g {
                slerp_gameimages::GameImageFormat::Xnb => "xnb",
                slerp_gameimages::GameImageFormat::Blp => "blp",
                slerp_gameimages::GameImageFormat::Pvr => "pvr",
                slerp_gameimages::GameImageFormat::Pkm => "pkm",
                slerp_gameimages::GameImageFormat::BuildArt => "build-art",
            },
            Kind::Metafile(m) => match m {
                slerp_metafile::MetafileFormat::Wmf => "wmf",
                slerp_metafile::MetafileFormat::Emf => "emf",
                slerp_metafile::MetafileFormat::Pict => "pict",
            },
            Kind::Volume(v) => match v {
                V::Npy | V::Npz => "numpy",
                V::Nifti if ext("hdr") || ext("img") => "analyze",
                V::Nifti => "nifti",
                V::MetaImage => "metaimage",
                V::Nrrd => "nrrd",
                V::Tiff => {
                    let candidates = slerp_formats::by_file_name(&name);
                    candidates.first().map(|f| f.id).filter(|id| *id != "tiff").unwrap_or("tiff")
                }
                V::NetCdf => "netcdf",
                // NetCDF-4 files are HDF5 inside.
                V::Hdf5 if ext("nc") => "netcdf",
                V::Hdf5 if ext("mat") => "matlab",
                V::Hdf5 => "hdf5",
            },
            Kind::Archive(a) => match a {
                A::DoomWad => "doom-wad",
                A::Zip if ext("pk4") => "idtech4-pk4",
                A::Zip => "doom-pk3",
                A::SevenZip => "doom-pk7",
                A::Pak => "quake-pak",
                A::Wad2 | A::Wad3 => "quake-wad",
                A::Pck => "godot-pck",
                A::Vpk => "valve-vpk",
                A::Grp => "build-grp",
                A::UnityPackage => "unity-package",
            },
            Kind::Image(i) => match i {
                I::Gif => "gif",
                I::Apng => "apng",
                I::AnimatedWebp => "webp",
                I::Tiff => "tiff",
                I::Ico => "ico",
                I::Pnm => "pnm",
                I::Jp2 => "jpeg2000",
                I::Jxl => "jxl",
                I::Avif => "avif",
                I::Heif => "heif",
                I::Qoi => "qoi",
                I::Icns => "icns",
                I::Sgi => "sgi",
                I::Xpm => "xpm",
                I::Xbm => "xbm",
                I::Exr => "openexr",
            },
            Kind::Paint(f) => match f {
                slerp_paint::PaintFormat::Psd => "psd",
                slerp_paint::PaintFormat::Xcf => "xcf",
                slerp_paint::PaintFormat::Ora => "openraster",
                slerp_paint::PaintFormat::Kra => "krita",
            },
            Kind::Svg => "svg",
            Kind::SketchUp => "sketchup",
            Kind::Document(d) => match d {
                slerp_documents::DocumentFormat::Pdf => "pdf",
                slerp_documents::DocumentFormat::Illustrator => "illustrator",
                slerp_documents::DocumentFormat::Eps => "eps",
                slerp_documents::DocumentFormat::PostScript => "postscript",
            },
            Kind::Minecraft(_) => "minecraft-schematic",
            Kind::Draco => "draco",
            Kind::Rigged(r) => match r {
                slerp_rigged::RiggedFormat::Pmx => "mmd-pmx",
                slerp_rigged::RiggedFormat::Pmd => "mmd-pmd",
                slerp_rigged::RiggedFormat::Vmd => "mmd-vmd",
                slerp_rigged::RiggedFormat::Md5Mesh => "md5mesh",
                slerp_rigged::RiggedFormat::Md5Anim => "md5anim",
                slerp_rigged::RiggedFormat::Iqm => "iqm",
                slerp_rigged::RiggedFormat::Iqe => "iqe",
                slerp_rigged::RiggedFormat::Smd => "source-smd",
                slerp_rigged::RiggedFormat::Psk => "unreal-psk",
                slerp_rigged::RiggedFormat::Psa => "unreal-psa",
            },
            Kind::Roblox(_) if ext("rbxm") || ext("rbxmx") => "roblox-model",
            Kind::Roblox(_) => "roblox-place",
            Kind::Maya(_) => "maya",
            Kind::Max => "3dsmax",
        })
    }
}

/// A PNG of 16-bit grey samples (with or without alpha), the data images
/// Slerp stretches.
fn png_16bit_grey(path: &Path) -> bool {
    let mut head = [0u8; 26];
    let read = std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut head));
    read.is_ok() && head.starts_with(b"\x89PNG") && head[24] == 16 && (head[25] == 0 || head[25] == 4)
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    /// PLY is meshes, point clouds and Gaussian splats alike: splat headers
    /// go to the splat loader, SuperSplat's compressed ones too, which the
    /// mesh loader would misread.
    #[test]
    fn plys_go_to_the_right_loader() {
        let mesh = b"ply\nformat ascii 1.0\nelement vertex 3\nproperty float x\nproperty float y\nproperty float z\nelement face 1\nproperty list uchar int vertex_indices\nend_header\n";
        assert!(matches!(sniff(Path::new("m.ply"), mesh), Some(Kind::Model(ModelFormat::Ply))));
        let splat = b"ply\nformat binary_little_endian 1.0\nelement vertex 1\nproperty float x\nproperty float f_dc_0\nend_header\n";
        assert_eq!(sniff(Path::new("s.ply"), splat), Some(Kind::Splat(slerp_splats::SplatFormat::Ply)));
        let compressed = b"ply\nformat binary_little_endian 1.0\nelement chunk 1\nproperty float min_x\nelement vertex 1\nproperty uint packed_position\nproperty uint packed_rotation\nend_header\n";
        assert_eq!(sniff(Path::new("c.ply"), compressed), Some(Kind::Splat(slerp_splats::SplatFormat::CompressedPly)));
        assert!(Kind::Splat(slerp_splats::SplatFormat::Spz).usually_3d());
    }

    /// Many formats end in `.map`; only Quake-family maps are claimed, by
    /// content, and every other loader leaves the rest alone too.
    #[test]
    fn only_quake_maps_are_claimed() {
        let quake = b"// Game: Quake\n// Format: Valve\n{\n\"classname\" \"worldspawn\"\n";
        assert_eq!(sniff(Path::new("e1m1.map"), quake), Some(Kind::Quake(QuakeFormat::Map)));
        let hammer = b"versioninfo\n{\n\t\"editorversion\" \"400\"\n}\n";
        assert_eq!(sniff(Path::new("d1.vmf"), hammer), Some(Kind::Quake(QuakeFormat::Vmf)));
        assert!(Kind::Quake(QuakeFormat::Map).usually_3d());
        let others: [&[u8]; 3] = [
            br#"{"version":3,"file":"furo.js","mappings":";iCAAA","sources":["a.js"],"names":["classname"]}"#,
            b"<map id=\"lib/strings.c\" name=\"lib/strings.c\">\n<area shape=\"rect\" href=\"x.html\"/>\n</map>\n",
            b"Archive member included to satisfy reference by file (symbol)\n\n/usr/lib/libc.a(printf.o)  main.o (printf)\n",
        ];
        for head in others {
            assert_eq!(sniff(Path::new("x.map"), head), None, "{}", String::from_utf8_lossy(head));
        }
    }
}
