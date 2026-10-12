//! Stored preview pictures (`crates/previews`): 3ds Max, SketchUp, Cinema
//! 4D, ZBrush, Procreate, Affinity, Sketch, Figma, Clip Studio, Paint.NET,
//! InDesign, XD, FreeCAD, CorelDRAW, Visio, DWG, DWF, SolidWorks, Inventor,
//! Revit, Solid Edge, Fusion, iWork, Paint Shop Pro, Pixelmator, SAI; each
//! one's parts; Visio's metafiles drawn as the core draws them.
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_fuzz::{Target, finish, finish_texture, head, split};
use std::sync::LazyLock;

static T: LazyLock<Target> = LazyLock::new(|| Target {
    name: "previews",
    exts: vec![
        "max", "skp", "c4d", "ztl", "zpr", "procreate", "afphoto", "afdesign", "sketch", "fig", "clip", "pdn", "indd",
        "xd", "fcstd", "cdr", "cmx", "vsd", "vsdx", "dwg", "dwf", "dwfx", "sldprt", "ipt", "rvt", "rfa", "psm", "dft",
        "f3d", "key", "pages", "numbers", "pspimage", "pxd", "sai", "sai2", "uasset", "pfr",
    ],
});

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    let path = input.file();
    let Some(format) = slerp_previews::sniff(&path, head(input.body)) else { return };
    let limits = input.limits();
    let size = input.flags.size();
    match slerp_previews::read_stored_path(format, &path, &limits) {
        Ok(slerp_previews::Stored::Image(tex)) => finish_texture(&tex, size),
        Ok(slerp_previews::Stored::Metafile(bytes)) => {
            if let Ok(tex) = slerp_metafile::load_sniffed(&bytes, &limits) {
                finish_texture(&tex, size);
            }
        }
        Err(_) => {}
    }
    let parts = slerp_previews::parts(format, &path);
    if let Some(part) = parts.get((input.flags.0 >> 4) as usize % parts.len().max(1))
        && let Ok(asset) = slerp_previews::load_part(format, &path, &limits, part)
    {
        finish(&asset, size);
    }
});
