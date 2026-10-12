//! MetaHuman DNA files: the head and body rigs MetaHuman Creator exports
//! (DCC Export writes `head.dna` and `body.dna` with a `Maps` folder).
//!
//! A DNA file holds a character's meshes at up to eight levels of detail,
//! its joints, its blend shapes, and the rig that turns some 250 raw
//! controls (`CTRL_expressions.jawOpen`, ...) into joint offsets and blend
//! shape weights.  Epic's openriglogic (MIT; `openriglogic/`, built by
//! `build.rs`) reads the file and runs that rig, RigLogic, here exactly as
//! Unreal runs it; `ffi` is the small cxx bridge to it.
//!
//! - [`load`] builds a scene for thumbnails and `slerp render`: the neutral
//!   head at a level of detail that suits the size.
//! - [`open`] gives the viewer a [`Character`]: the scene at any level of
//!   detail, with the body that goes with a head, the textures from the
//!   maps folder, and the rigs, which [`Character::apply`] evaluates into a
//!   pose every time it is recomputed.
//!
//! openriglogic's reader is C++ that trusts some of what a file says, so
//! files are checked before it sees them and, on Unix, read in a child
//! process (see `sandbox`).

mod arkit;
mod cache;
mod extract;
mod ffi;
mod fit;
mod maps;
mod rig;
mod sandbox;
mod scene;
mod wire;

pub use arkit::{ARKIT_CONTROLS, arkit_controls, control_group};
pub use rig::{Rig, RigKind};

use extract::{DnaData, Request};
use slerp_scene::{Limits, LoadError, Pose, Scene};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/*┌─────────────────╮
| Recognising     |
└─────────────────╯*/
//region Recognising

/// Whether a file's first bytes are a DNA file's: "DNA", generation 2 and
/// a version, all big-endian, as openriglogic writes them.
pub fn sniff(head: &[u8]) -> bool {
    head.len() >= 7
        && &head[..3] == b"DNA"
        && head[3..5] == [0, 2]
        && (1..=64).contains(&u16::from_be_bytes([head[5], head[6]]))
}

/// Checks a DNA file's header and its index of sections against the file's
/// length before openriglogic reads it, so a truncated file or one whose
/// index points past its end is refused here.
fn check_header(path: &Path, max_bytes: u64) -> Result<u64, LoadError> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    if len > max_bytes {
        return Err(LoadError::TooLarge(format!("{len} bytes")));
    }
    let mut head = Vec::with_capacity(11 + 16 * 64);
    (&mut file).take(11 + 16 * 64).read_to_end(&mut head)?;
    if !sniff(&head) {
        return Err(LoadError::NotMine);
    }
    if len < 11 || head.len() < 11 {
        return Err(LoadError::invalid("the file ends inside its header"));
    }
    let version = u16::from_be_bytes([head[5], head[6]]);
    // Version 1 is the older single-block layout; later ones list their
    // sections, each as an id, a version, an offset and a size.
    if version >= 2 {
        let be = |at: usize| u32::from_be_bytes([head[at], head[at + 1], head[at + 2], head[at + 3]]) as u64;
        let count = be(7) as usize;
        if count > 64 {
            return Err(LoadError::invalid(format!("{count} sections")));
        }
        if head.len() < 11 + 16 * count {
            return Err(LoadError::invalid("the file ends inside its section index"));
        }
        for i in 0..count {
            let at = 11 + 16 * i;
            let (offset, size) = (be(at + 8), be(at + 12));
            if offset.saturating_add(size) > len {
                return Err(LoadError::invalid(format!(
                    "section {} runs past the end of the file",
                    String::from_utf8_lossy(&head[at..at + 4])
                )));
            }
        }
    }
    Ok(len)
}

//endregion

/*┌─────────────────╮
| Options         |
└─────────────────╯*/
//region Options

/// What to load.
#[derive(Clone, Debug)]
pub struct Options {
    /// Level of detail, 0 (finest) to 7 for heads and 3 for bodies; each
    /// file uses its own coarsest when it has fewer.
    pub lod: u16,
    /// Blend shapes as morph targets.  MetaHuman heads have them at level
    /// 0 only; coarser levels move by joints alone, as in Unreal.
    pub blend_shapes: bool,
    /// Load the body beside a head (the head beside a body is always
    /// loaded, since a body alone has no face).
    pub body: bool,
    /// Textures from the maps folder beside the file, no larger than
    /// `max_texture_size`.
    pub textures: bool,
    pub max_texture_size: Option<u32>,
    /// Keep the meshes that need Unreal's own shaders, hidden.
    pub helpers: bool,
    /// Build the rigs (the viewer).
    pub rig: bool,
    pub max_triangles: u64,
    pub max_file_bytes: u64,
    /// How long reading may take before the reader is stopped.
    pub timeout: Duration,
}

impl Options {
    /// The neutral head for a thumbnail of `size` pixels, at a level of
    /// detail fine enough for that size, with the textures beside the file
    /// shrunk to that size (clay with painted eyes when there are none, as
    /// in GNOME's sandbox).
    pub fn thumbnail(size: u32) -> Options {
        let lod = match size {
            0..=256 => 3,
            257..=512 => 2,
            _ => 1,
        };
        Options {
            lod,
            blend_shapes: false,
            body: false,
            textures: true,
            max_texture_size: Some(size),
            helpers: false,
            rig: false,
            max_triangles: Limits::thumbnail().max_triangles,
            max_file_bytes: Limits::thumbnail().max_file_bytes,
            timeout: Duration::from_secs(10),
        }
    }

    /// Everything, for the viewer: full detail with blend shapes, the
    /// body, textures and the rigs.
    pub fn viewer() -> Options {
        Options {
            lod: 0,
            blend_shapes: true,
            body: true,
            textures: true,
            max_texture_size: None,
            helpers: true,
            rig: true,
            max_triangles: u64::MAX,
            max_file_bytes: u64::MAX,
            timeout: Duration::from_secs(60),
        }
    }

    /// What core's loader asks for: thumbnail limits (a texture size cap)
    /// give the thumbnail head; the viewer's limits give the whole
    /// character without the rigs, which only the viewer drives.
    pub fn from_limits(limits: &Limits) -> Options {
        let mut opts = match limits.max_texture_size {
            Some(size) => Options::thumbnail(size),
            None => Options { rig: false, ..Options::viewer() },
        };
        opts.max_triangles = limits.max_triangles;
        opts.max_file_bytes = limits.max_file_bytes;
        opts.textures &= limits.neighbours;
        opts.body &= limits.neighbours;
        opts
    }
}

//endregion

/*┌─────────────────╮
| Loading         |
└─────────────────╯*/
//region Loading

/// Loads a DNA file as a scene, as `limits` asks (see
/// [`Options::from_limits`]).
pub fn load(path: &Path, limits: &Limits) -> Result<Scene, LoadError> {
    Ok(open(path, &Options::from_limits(limits))?.scene)
}

/// A MetaHuman read from its DNA files: the data at every level of detail
/// that was read, its textures, the scene built at one level, and the rigs
/// that pose it.
pub struct Character {
    pub path: PathBuf,
    /// The head, or a body opened without one.
    main: DnaData,
    /// The body loaded with a head.
    body: Option<DnaData>,
    maps: Option<maps::Maps>,
    opts: Options,
    /// Whether the file opened was the head rather than the body.
    opened_head: bool,
    lod: u16,
    pub scene: Scene,
    /// The face rig (or a lone body's rig).
    pub head_rig: Option<Rig>,
    /// The body's correctives, when a body was loaded with a head.
    pub body_rig: Option<Rig>,
    /// Problems worked around, such as a missing partner file.
    pub warnings: Vec<String>,
    /// Milliseconds spent reading, decoding textures, and building.
    pub timings: Vec<(String, f64)>,
    /// Names what was read, for the refits kept in the cache (see `cache`).
    cache_key: Option<cache::Key>,
}

impl Character {
    /// Levels of detail the head (or lone body) has.
    pub fn lod_count(&self) -> u16 {
        self.main.lod_count.max(1)
    }

    pub fn lod(&self) -> u16 {
        self.lod
    }

    /// Whether the main file is a head with face controls.
    pub fn has_face(&self) -> bool {
        scene::is_head(&self.main)
    }

    /// Whether a body was loaded with the head.
    pub fn body_loaded(&self) -> bool {
        self.body.is_some()
    }

    /// Rebuilds the scene at level of detail `lod` and points the rigs at
    /// it.  Nodes keep their indices, so clips added to the old scene can
    /// be moved to the new one.
    pub fn set_lod(&mut self, lod: u16) -> Result<(), LoadError> {
        let lod = lod.min(self.lod_count() - 1);
        let built = scene::build(&self.main, self.body.as_ref(), &self.build_options(lod))?;
        self.scene = built.scene;
        self.scene.name = character_name(&self.path, &self.main);
        self.lod = lod;
        if let Some(r) = &mut self.head_rig {
            r.retarget(built.head_joints, built.channel_targets);
            r.set_lod(lod);
        }
        if let Some(r) = &mut self.body_rig {
            r.retarget(built.body_joints, Vec::new());
            r.set_lod(lod);
        }
        self.refit(&built.sources);
        Ok(())
    }

    /// Refits the head's skin weights to four joints per vertex against
    /// the face rig (see `fit`), and notes how well they follow.
    fn refit(&mut self, sources: &[scene::MeshSource]) {
        let Some(rig) = self.head_rig.as_mut().filter(|r| r.kind == RigKind::Head) else { return };
        let start = Instant::now();
        // The fit depends on the files and the level of detail, which also
        // decides the meshes it applies to.
        let key = self.cache_key.map(|k| {
            let meshes: Vec<u16> = sources.iter().filter(|s| s.main).map(|s| s.dna_mesh).collect();
            k.with(format!("fit {} {meshes:?}", self.lod).as_bytes())
        });
        let cached = key.and_then(|k| cache::read(k, "fit")).and_then(|b| fit::Fit::from_bytes(&b));
        let (fit, label) = match cached {
            Some(f) => (f, "Skin weights from the cache"),
            None => {
                let f = fit::refit(&self.scene, sources, &self.main, rig);
                if let Some(k) = key {
                    cache::write(k, "fit", f.to_bytes());
                }
                (f, "Fitting skin weights")
            }
        };
        fit::apply(&mut self.scene, sources, &fit);
        let r = fit.report;
        self.timings.push((label.to_string(), start.elapsed().as_secs_f64() * 1e3));
        if r.vertices > 0 {
            self.scene.info.details.push((
                "Skin weights".into(),
                format!(
                    "{} vertices refitted to 4 joints over {} poses: off by {:.2} mm on average, at most {:.1} mm \
                     ({:.2} and {:.1} mm keeping the strongest 4)",
                    r.vertices,
                    r.poses,
                    r.mean_after * 1e3,
                    r.max_after * 1e3,
                    r.mean_before * 1e3,
                    r.max_before * 1e3
                ),
            ));
        }
    }

    fn build_options(&self, lod: u16) -> scene::Build<'_> {
        scene::Build {
            lod,
            blend_shapes: self.opts.blend_shapes,
            helpers: self.opts.helpers,
            maps: self.maps.as_ref(),
            max_triangles: self.opts.max_triangles,
            opened_head: self.opened_head,
        }
    }

    /// Evaluates the rigs into `pose` (local transforms and morph weights,
    /// before `Pose::update_world`): the body's correctives from the joint
    /// rotations a clip left, then the face from its controls.
    pub fn apply(&mut self, pose: &mut Pose) -> Result<(), String> {
        if let Some(r) = &mut self.body_rig {
            r.apply(pose)?;
        }
        if let Some(r) = &mut self.head_rig {
            r.apply(pose)?;
        }
        Ok(())
    }
}

/// Reads `path` (and the files that go with it) as `opts` says.
pub fn open(path: &Path, opts: &Options) -> Result<Character, LoadError> {
    let mut timings = Vec::new();
    let mut warnings = Vec::new();
    let start = Instant::now();

    // 1. The file and its partner, with the textures read meanwhile: the
    //    textures are decoded in parallel in this process, the files read
    //    in a child process (or from the cache).
    let (read, maps) = rayon::join(
        || read_files(path, opts),
        || {
            let start = Instant::now();
            let maps = opts
                .textures
                .then(|| maps::find_folder(path))
                .flatten()
                .map(|folder| maps::load(&folder, opts.max_texture_size));
            (maps, start.elapsed().as_secs_f64() * 1e3)
        },
    );
    let Files { main, main_path, body, body_path, mut neighbours, cache_key, partner_warning, first_is_head } = read?;
    warnings.extend(partner_warning);
    timings.push(("Reading".to_string(), start.elapsed().as_secs_f64() * 1e3));
    let (maps, textures_ms) = maps;
    if let Some(m) = &maps {
        warnings.extend(m.warnings.iter().cloned());
        neighbours.extend(m.files.iter().cloned());
        timings.push(("Textures (while reading)".to_string(), textures_ms));
    }

    // 4. The scene.  When only one level was read it is level 0 of what
    //    was read.
    let start = Instant::now();
    let mut character = Character {
        path: path.to_path_buf(),
        main,
        body,
        maps,
        opts: opts.clone(),
        opened_head: first_is_head,
        lod: 0,
        scene: Scene::default(),
        head_rig: None,
        body_rig: None,
        warnings,
        timings,
        cache_key,
    };
    // The viewer (`rig`) reads every level; otherwise only the one wanted
    // was read, as level 0.
    let lod = if opts.rig { opts.lod.min(character.lod_count() - 1) } else { 0 };
    let built = scene::build(&character.main, character.body.as_ref(), &character.build_options(lod))?;
    character.scene = built.scene;
    character.scene.name = character_name(path, &character.main);
    character.lod = lod;
    character.timings.push(("Building".to_string(), start.elapsed().as_secs_f64() * 1e3));

    // 5. The rigs, built in this process from the files the child
    //    process has just read and evaluated without harm.
    if opts.rig {
        let start = Instant::now();
        let kind = if scene::is_head(&character.main) { RigKind::Head } else { RigKind::Body };
        character.head_rig = Some(make_rig(
            &main_path,
            kind,
            &character.main,
            built.head_joints,
            built.channel_targets,
            &character.scene,
            lod,
        )?);
        if let (Some(b), Some(p)) = (&character.body, &body_path) {
            match make_rig(p, RigKind::Body, b, built.body_joints, Vec::new(), &character.scene, lod) {
                Ok(r) => character.body_rig = Some(r),
                Err(e) => character.warnings.push(format!("body rig: {e}")),
            }
        }
        character.timings.push(("Rig".to_string(), start.elapsed().as_secs_f64() * 1e3));
        character.refit(&built.sources);
    }
    let info = &mut character.scene.info;
    info.warnings.extend(character.warnings.iter().cloned());
    info.neighbours = neighbours;
    Ok(character)
}

fn make_rig(
    path: &Path,
    kind: RigKind,
    data: &DnaData,
    joints: Vec<usize>,
    channels: Vec<Vec<(usize, usize)>>,
    scene: &Scene,
    lod: u16,
) -> Result<Rig, LoadError> {
    let bytes = std::fs::metadata(path)?.len();
    let dna = ffi::bridge::read_dna(
        &extract::path_bytes(path).map_err(LoadError::Invalid)?,
        extract::layer::RIG,
        &[],
        extract::budget(bytes),
    )
    .map_err(|e| LoadError::Invalid(e.to_string()))?;
    let dna = dna.as_ref().ok_or_else(|| LoadError::invalid("openriglogic returned no reader"))?;
    let mut rig = Rig::new(dna, extract::budget(bytes), kind, scene::Units::of(data), joints, channels, scene)
        .map_err(LoadError::Invalid)?;
    rig.set_lod(lod);
    Ok(rig)
}

/// What to call a character: the name the file gives, unless it is
/// MetaHuman Creator's placeholder ("Archetype") or missing; then the
/// folder's name for DCC Export's `head.dna` and `body.dna`, else the file's.
fn character_name(path: &Path, d: &DnaData) -> String {
    if !d.name.trim().is_empty() && d.name != "Archetype" {
        return d.name.clone();
    }
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let folder = path.parent().and_then(|p| p.file_name()).map(|s| s.to_string_lossy().into_owned());
    match folder {
        Some(f) if matches!(stem.to_ascii_lowercase().as_str(), "head" | "body") => f,
        _ => stem,
    }
}

/// The file that completes a character: `body.dna` beside `head.dna` and
/// the other way round, matching the name's case.
fn partner_path(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_str()?;
    let lower = name.to_ascii_lowercase();
    let (from, to) = if lower.contains("head") {
        ("head", "body")
    } else if lower.contains("body") {
        ("body", "head")
    } else {
        return None;
    };
    let at = lower.find(from)?;
    let original = &name[at..at + 4];
    let replacement: String = original
        .chars()
        .zip(to.chars())
        .map(|(o, t)| if o.is_ascii_uppercase() { t.to_ascii_uppercase() } else { t })
        .collect();
    let other = path.with_file_name(format!("{}{}{}", &name[..at], replacement, &name[at + 4..]));
    other.is_file().then_some(other)
}

/// The DNA files of a character, read.
struct Files {
    /// The head, or a body opened without one.
    main: DnaData,
    main_path: PathBuf,
    body: Option<DnaData>,
    body_path: Option<PathBuf>,
    /// The partner file, for watching.
    neighbours: Vec<PathBuf>,
    /// What was read, for the cache, when every file read was cached.
    cache_key: Option<cache::Key>,
    partner_warning: Option<String>,
    /// Whether the file opened is a head (rather than a body).
    first_is_head: bool,
}

/// Reads the file at `path` and the one that completes the character: the
/// body beside a head when asked, the head beside a body always.
fn read_files(path: &Path, opts: &Options) -> Result<Files, LoadError> {
    // 1. The file itself: every level when the viewer may switch, else
    //    only the level wanted.
    let lods: Vec<u16> = if opts.rig { Vec::new() } else { vec![opts.lod] };
    let request = Request { lods, geometry: true, blend_shapes: opts.blend_shapes, probe_rig: opts.rig };
    let (first, first_key) = read(path, &request, opts)?;
    let first_is_head = scene::is_head(&first);

    // 2. Its partner.
    let mut partner = None;
    let mut neighbours = Vec::new();
    let mut partner_warning = None;
    let mut cache_key = first_key;
    if (opts.body || !first_is_head)
        && let Some(other) = partner_path(path)
    {
        neighbours.push(other.clone());
        // Read at the same levels as the file opened.
        match read(&other, &request, opts) {
            Ok((d, key)) if scene::is_head(&d) != first_is_head => {
                partner = Some((other, d));
                cache_key = cache_key.zip(key).map(|(a, b)| a.with(&b.0.to_le_bytes()));
            }
            Ok(_) => {}
            Err(e) => partner_warning = Some(format!("{}: {e}", other.display())),
        }
    }
    let (main, main_path, body, body_path) = match partner {
        Some((other, d)) if !first_is_head => (d, other, Some(first), Some(path.to_path_buf())),
        Some((other, d)) => (first, path.to_path_buf(), Some(d), Some(other)),
        None => (first, path.to_path_buf(), None, None),
    };
    Ok(Files { main, main_path, body, body_path, neighbours, cache_key, partner_warning, first_is_head })
}

/// Reads one DNA file: checks its header, then has openriglogic read it in
/// a child process on Unix (in this one elsewhere, or when
/// `SLERP_DNA_IN_PROCESS` is set, for profiling).  The viewer's reads
/// (which build the rigs) are kept in the cache and come from it when the
/// file hasn't changed; the key that names them comes back too.
fn read(path: &Path, request: &Request, opts: &Options) -> Result<(DnaData, Option<cache::Key>), LoadError> {
    let len = check_header(path, opts.max_file_bytes)?;
    let what = format!("{:?} {} {} {}", request.lods, request.geometry, request.blend_shapes, request.probe_rig);
    let key = if opts.rig { cache::Key::of_file(path, what.as_bytes()) } else { None };
    if let Some(k) = key
        && let Some(bytes) = cache::read(k, "data")
        && let Ok(d) = wire::decode(&bytes)
    {
        return Ok((d, key));
    }
    if cfg!(not(unix)) || std::env::var_os("SLERP_DNA_IN_PROCESS").is_some() {
        let d = extract::extract(path, request).map_err(LoadError::Invalid)?;
        if let Some(k) = key {
            cache::write(k, "data", wire::encode(&d));
        }
        return Ok((d, key));
    }
    let max_answer = (len as usize).saturating_mul(8).saturating_add(64 << 20);
    let (p, r) = (path.to_path_buf(), request.clone());
    let answer =
        sandbox::isolated(move || extract::extract(&p, &r).map(|d| wire::encode(&d)), opts.timeout, max_answer)
            .map_err(LoadError::Invalid)?;
    let d = wire::decode(&answer).map_err(LoadError::Invalid)?;
    if let Some(k) = key {
        cache::write(k, "data", answer);
    }
    Ok((d, key))
}

//endregion

/*┌─────────────────╮
| Portraits       |
└─────────────────╯*/
//region Portraits

/// How to frame a DNA head as a portrait: nearly level, a little from the
/// side, the head and neck filling a square picture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Portrait {
    /// Degrees around the vertical from the front, and above the horizon.
    pub yaw: f32,
    pub pitch: f32,
    /// The point to look at and the radius around it to fit in view.
    pub target: glam::Vec3,
    pub radius: f32,
}

/// The portrait framing of a scene this crate built from a head file, with
/// or without its body, or `None` for any other scene (a body file is
/// better framed whole).  The frame runs
/// from just above the top of the head down the neck, as far below the
/// head joint as 0.8 times the joint's depth under the crown, so the face
/// fills the picture and the frame's edges crop the shoulders.
pub fn portrait(scene: &Scene) -> Option<Portrait> {
    if scene.info.format != scene::HEAD_FORMAT && scene.info.format != scene::HEAD_WITH_BODY_FORMAT {
        return None;
    }
    let pose = Pose::rest(scene);
    let top = scene.bounds(&pose).max.y;
    if !top.is_finite() {
        return None;
    }
    let head_y = scene
        .nodes
        .iter()
        .position(|n| n.name == "head" && n.mesh.is_none())
        .map(|i| pose.world[i].w_axis.y)
        .filter(|&y| y < top && top - y < 0.5)
        .unwrap_or(top - 0.2);
    let depth = top - head_y;
    let (low, high) = (head_y - 0.8 * depth, top + 0.1 * depth);
    // The skull, above the head joint, gives the middle from side to side
    // and front to back.  Skinned meshes are at rest, where skinning is the
    // identity.
    let mut skull = slerp_scene::Aabb::EMPTY;
    for (ni, node) in scene.nodes.iter().enumerate() {
        let (Some(mesh), true) = (node.mesh.and_then(|m| scene.meshes.get(m)), node.visible) else { continue };
        for &p in mesh.primitives.iter().flat_map(|p| &p.positions) {
            let p = if node.skin.is_some() { p } else { pose.world[ni].transform_point3(p) };
            if p.y >= head_y {
                skull.add(p);
            }
        }
    }
    (!skull.is_empty()).then(|| {
        let c = skull.center();
        Portrait {
            yaw: 22.0,
            pitch: 6.0,
            target: glam::Vec3::new(c.x, (low + high) * 0.5, c.z),
            radius: (high - low) * 0.5,
        }
    })
}

//endregion

#[cfg(test)]
mod tests;
