//! Decoding real files against the references the sample sets provide:
//! the same image stored uncompressed and with each codec.  Lossless codecs
//! must match the reference exactly; lossy ones closely.  Files come from
//! `local/assets/dicom/` (`tools/fetch-test-assets.ts dicom`) and the
//! local-only `local/assets/dicom-extra/`; tests skip what is missing.

use slerp_dicom::{Dicom, Frame, FramePixels};
use std::path::{Path, PathBuf};

fn assets() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets")
}

fn find(rel: &str) -> Option<PathBuf> {
    let p = assets().join(rel);
    if p.exists() {
        Some(p)
    } else {
        eprintln!("skipping: {} is missing", p.display());
        None
    }
}

fn frames(path: &Path) -> Vec<Frame> {
    let d = Dicom::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    (0..d.frame_count())
        .map(|i| d.decode_frame(i, None).unwrap_or_else(|e| panic!("{} frame {i}: {e}", path.display())))
        .collect()
}

/// Largest and mean absolute difference between two decodings.
fn difference(a: &[Frame], b: &[Frame]) -> (f64, f64) {
    assert_eq!(a.len(), b.len(), "frame counts differ");
    let (mut max, mut sum, mut n) = (0f64, 0f64, 0usize);
    for (fa, fb) in a.iter().zip(b) {
        assert_eq!((fa.width, fa.height), (fb.width, fb.height), "sizes differ");
        let mut add = |x: f64, y: f64| {
            let d = (x - y).abs();
            max = max.max(d);
            sum += d;
            n += 1;
        };
        match (&fa.pixels, &fb.pixels) {
            (FramePixels::Grey(x), FramePixels::Grey(y)) => {
                x.iter().zip(y).for_each(|(&p, &q)| add(p as f64, q as f64))
            }
            (FramePixels::Rgb(x), FramePixels::Rgb(y)) => x.iter().zip(y).for_each(|(&p, &q)| add(p as f64, q as f64)),
            _ => panic!("one is colour, the other greyscale"),
        }
    }
    (max, sum / n.max(1) as f64)
}

/// Each file must decode exactly like the first.
fn same(files: &[&str]) {
    let Some(reference) = find(files[0]) else { return };
    let want = frames(&reference);
    for f in &files[1..] {
        let Some(p) = find(f) else { continue };
        let (max, _) = difference(&want, &frames(&p));
        assert_eq!(max, 0.0, "{f} differs from {}", files[0]);
    }
}

/// The spread of a decoding's values (255 for colour).
fn value_range(frames: &[Frame]) -> f64 {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for f in frames {
        match &f.pixels {
            FramePixels::Grey(v) => v.iter().for_each(|&x| {
                lo = lo.min(x as f64);
                hi = hi.max(x as f64);
            }),
            FramePixels::Rgb(_) => return 255.0,
        }
    }
    (hi - lo).max(1.0)
}

/// Lossy files must stay within `max_percent` of the reference's value
/// range on average.
fn close(reference: &str, lossy: &str, max_percent: f64) {
    let (Some(r), Some(l)) = (find(reference), find(lossy)) else { return };
    let want = frames(&r);
    let (max, mean) = difference(&want, &frames(&l));
    let percent = 100.0 * mean / value_range(&want);
    eprintln!("{lossy}: mean difference {mean:.3} ({percent:.2}% of the range), largest {max}");
    assert!(percent <= max_percent, "{lossy}: mean difference {percent:.2}% of the range from {reference}");
}

/*┌─────────────────╮
| pydicom         |
└─────────────────╯*/
//region pydicom

#[test]
fn encodings_of_one_mr_image() {
    same(&[
        "dicom/pydicom-test-files/MR_small.dcm",
        "dicom/pydicom-test-files/MR_small_implicit.dcm",
        "dicom/pydicom-test-files/MR_small_bigendian.dcm",
        "dicom/pydicom-test-files/MR_small_expb.dcm",
        "dicom/pydicom-test-files/MR_small_RLE.dcm",
        "dicom/pydicom-test-files/MR_small_jpeg_ls_lossless.dcm",
        "dicom/pydicom-test-files/MR_small_jp2klossless.dcm",
    ]);
}

#[test]
fn multi_frame_mr_in_every_lossless_codec() {
    same(&[
        "dicom/pydicom-data/emri_small.dcm",
        "dicom/pydicom-data/emri_small_big_endian.dcm",
        "dicom/pydicom-data/emri_small_RLE.dcm",
        "dicom/pydicom-data/emri_small_jpeg_ls_lossless.dcm",
        "dicom/pydicom-data/emri_small_jpeg_2k_lossless.dcm",
    ]);
    if let Some(p) = find("dicom/pydicom-data/emri_small.dcm") {
        assert_eq!(Dicom::open(&p).unwrap().frame_count(), 10);
    }
}

#[test]
fn rgb_in_every_encoding() {
    same(&[
        "dicom/pydicom-data/SC_rgb.dcm",
        "dicom/pydicom-data/SC_rgb_expb.dcm",
        "dicom/pydicom-test-files/SC_rgb_rle.dcm",
        "dicom/pydicom-data/SC_rgb_gdcm2k_uncompressed.dcm",
        "dicom/pydicom-test-files/SC_rgb_jpeg_gdcm.dcm",
    ]);
    same(&[
        "dicom/pydicom-data/SC_rgb_2frame.dcm",
        "dicom/pydicom-data/SC_rgb_expb_2frame.dcm",
        "dicom/pydicom-test-files/SC_rgb_rle_2frame.dcm",
    ]);
    same(&[
        "dicom/pydicom-data/SC_rgb_16bit.dcm",
        "dicom/pydicom-data/SC_rgb_expb_16bit.dcm",
        "dicom/pydicom-test-files/SC_rgb_rle_16bit.dcm",
    ]);
    same(&[
        "dicom/pydicom-data/SC_rgb_32bit_2frame.dcm",
        "dicom/pydicom-data/SC_rgb_expb_32bit_2frame.dcm",
        "dicom/pydicom-test-files/SC_rgb_rle_32bit_2frame.dcm",
    ]);
    same(&[
        "dicom/pydicom-test-files/SC_rgb_small_odd.dcm",
        "dicom/pydicom-test-files/SC_rgb_small_odd_big_endian.dcm",
    ]);
}

/// YBR_FULL and YBR_FULL_422 written uncompressed by GDCM from the same
/// JPEG: the colour conversion lands within rounding of the RGB.
#[test]
fn ybr_uncompressed() {
    close("dicom/pydicom-data/SC_rgb.dcm", "dicom/pydicom-data/SC_ybr_full_uncompressed.dcm", 1.0);
    close("dicom/pydicom-data/SC_rgb.dcm", "dicom/pydicom-test-files/SC_ybr_full_422_uncompressed.dcm", 2.0);
}

/// Baseline JPEG in YBR_FULL, YBR_FULL_422 and RGB (with and without
/// Adobe's marker): all close to the uncompressed original.
#[test]
fn jpeg_baseline_colour() {
    for f in [
        "dicom/pydicom-test-files/SC_rgb_jpeg_dcmtk.dcm",
        "dicom/pydicom-test-files/SC_rgb_dcmtk_+eb+cr.dcm",
        "dicom/pydicom-test-files/SC_rgb_dcmtk_+eb+cy+n1.dcm",
        "dicom/pydicom-test-files/SC_rgb_dcmtk_+eb+cy+n2.dcm",
        "dicom/pydicom-test-files/SC_rgb_dcmtk_+eb+cy+np.dcm",
        "dicom/pydicom-test-files/SC_rgb_dcmtk_+eb+cy+s2.dcm",
        "dicom/pydicom-test-files/SC_rgb_dcmtk_+eb+cy+s4.dcm",
        "dicom/pydicom-test-files/SC_rgb_jpeg_lossy_gdcm.dcm",
    ] {
        close("dicom/pydicom-data/SC_rgb.dcm", f, 1.5);
    }
    close("dicom/pydicom-test-files/SC_rgb_jpeg_dcmd.dcm", "dicom/pydicom-test-files/SC_rgb_jpeg.dcm", 0.5);
    // RGB JPEG without a colour transform (slide tiles, almost white): a
    // wrong YCbCr conversion would tint them green or magenta.
    for f in [
        "dicom/pydicom-test-files/SC_jpeg_no_color_transform.dcm",
        "dicom/pydicom-test-files/SC_jpeg_no_color_transform_2.dcm",
    ] {
        let Some(p) = find(f) else { continue };
        let FramePixels::Rgb(px) = &frames(&p)[0].pixels else { panic!("{f} is colour") };
        let n = (px.len() / 3) as f64;
        let mean = |c: usize| px.iter().skip(c).step_by(3).map(|&v| v as f64).sum::<f64>() / n;
        let (r, g, b) = (mean(0), mean(1), mean(2));
        assert!(r > 200.0 && (r - g).abs() < 12.0 && (g - b).abs() < 12.0, "{f}: mean colour {r:.0} {g:.0} {b:.0}");
    }
}

#[test]
fn jpeg_ls() {
    same(&[
        "dicom/pydicom-data/JLSL_RGB_ILV0.dcm",
        "dicom/pydicom-data/JLSL_RGB_ILV1.dcm",
        "dicom/pydicom-data/JLSL_RGB_ILV2.dcm",
    ]);
    close("dicom/pydicom-data/JLSL_RGB_ILV0.dcm", "dicom/pydicom-data/JLSN_RGB_ILV0.dcm", 1.5);
    close("dicom/pydicom-data/SC_rgb.dcm", "dicom/pydicom-test-files/SC_rgb_jls_lossy_line.dcm", 1.5);
    close("dicom/pydicom-data/SC_rgb.dcm", "dicom/pydicom-test-files/SC_rgb_jls_lossy_sample.dcm", 1.5);
}

#[test]
fn jpeg_2000_against_references() {
    same(&["dicom/pydicom-data/MR2_UNCR.dcm", "dicom/pydicom-data/MR2_J2KR.dcm"]);
    same(&["dicom/pydicom-data/RG3_UNCR.dcm", "dicom/pydicom-data/RG3_J2KR.dcm"]);
    same(&["dicom/pydicom-data/US1_UNCR.dcm", "dicom/pydicom-data/US1_J2KR.dcm"]);
    same(&["dicom/pydicom-data/693_UNCR.dcm", "dicom/pydicom-data/693_J2KR.dcm"]);
    // Irreversible JPEG 2000 against another decoder's output of it.
    close("dicom/pydicom-data/JPEG2000_UNC.dcm", "dicom/pydicom-test-files/JPEG2000.dcm", 0.05);
    close("dicom/pydicom-data/MR2_UNCR.dcm", "dicom/pydicom-data/MR2_J2KI.dcm", 1.0);
    close("dicom/pydicom-data/US1_UNCR.dcm", "dicom/pydicom-data/US1_J2KI.dcm", 1.0);
}

#[test]
fn one_bit_segmentations() {
    same(&[
        "dicom/pydicom-data/liver.dcm",
        "dicom/pydicom-data/liver_expb.dcm",
        "dicom/pydicom-data/liver_rle.dcm",
        "dicom/pydicom-data/liver_deflate.dcm",
        "dicom/pydicom-data/liver_j2k.dcm",
    ]);
    same(&[
        "dicom/pydicom-data/liver_nonbyte_aligned.dcm",
        "dicom/pydicom-data/liver_nonbyte_aligned_rle.dcm",
        "dicom/pydicom-data/liver_nonbyte_aligned_deflate.dcm",
        "dicom/pydicom-data/liver_nonbyte_aligned_j2k.dcm",
    ]);
}

#[test]
fn palettes() {
    same(&[
        "dicom/pydicom-data/OBXXXX1A.dcm",
        "dicom/pydicom-data/OBXXXX1A_expb.dcm",
        "dicom/pydicom-data/OBXXXX1A_rle.dcm",
    ]);
    same(&["dicom/pydicom-data/gdcm-US-ALOKA-16.dcm", "dicom/pydicom-data/gdcm-US-ALOKA-16_big.dcm"]);
    // The segmented palette isn't flat black.
    if let Some(p) = find("dicom/pydicom-data/gdcm-US-ALOKA-16.dcm") {
        let f = &frames(&p)[0];
        let FramePixels::Rgb(px) = &f.pixels else { panic!("palette images are colour") };
        let bright = px.iter().filter(|&&v| v > 64).count();
        assert!(bright > px.len() / 20, "{bright} bright samples");
    }
}

#[test]
fn rt_dose_32_bit() {
    same(&[
        "dicom/pydicom-test-files/rtdose.dcm",
        "dicom/pydicom-test-files/rtdose_expb.dcm",
        "dicom/pydicom-test-files/rtdose_rle.dcm",
    ]);
}

/// 12-bit lossy JPEG (process 4), with this crate's own decoder: pydicom's
/// two copies of NEMA's NM1_JPLY agree, and match NEMA's uncompressed NM1
/// closely (`nema_compressed_samples` checks the rest of NEMA's set).
#[test]
fn jpeg_12_bit() {
    same(&["dicom/pydicom-test-files/JPEG-lossy.dcm", "dicom/pydicom-test-files/JPGExtended.dcm"]);
    close("dicom-extra/nema-wg04/IMAGES/REF/NM1_UNC", "dicom/pydicom-test-files/JPGExtended.dcm", 1.0);
    // Reduced decoding for thumbnails.
    if let Some(p) = find("dicom/pydicom-test-files/JPGExtended.dcm") {
        let d = Dicom::open(&p).unwrap();
        let small = d.decode_frame(0, Some(16)).unwrap();
        assert!(small.width <= 64, "{}", small.width);
    }
}

/// Lossless JPEG (process 14): pydicom's copy of NEMA's NM1_JPLL.
#[test]
fn jpeg_lossless() {
    same(&["dicom-extra/nema-wg04/IMAGES/REF/NM1_UNC", "dicom/pydicom-data/JPEG-LL.dcm"]);
}

/// The stored VOI LUT and Modality LUT files of IHE's display tests.
#[test]
fn luts() {
    for f in ["dicom/pydicom-data/vlut_04.dcm", "dicom/pydicom-data/mlut_18.dcm"] {
        let Some(p) = find(f) else { continue };
        let d = Dicom::open(&p).unwrap();
        let frame = d.decode_frame(0, None).unwrap();
        let voi = d.default_voi(0, &frame);
        if f.contains("vlut") {
            assert!(matches!(voi, slerp_dicom::Voi::Lut(_)), "{f}: {voi:?}");
        } else {
            assert!(d.modality_transform(0).lut.is_some(), "{f}");
        }
        let pic = d.picture(&frame, &voi, d.inverted(), 1.0);
        let slerp_scene::textures::Pixels::Rgba8(px) = pic.base() else { panic!() };
        let (lo, hi) = px.chunks_exact(4).fold((255u8, 0u8), |(lo, hi), p| (lo.min(p[0]), hi.max(p[0])));
        assert!(hi - lo > 100, "{f}: grey range {lo}..{hi}");
    }
}

#[test]
fn dicomdirs() {
    let Some(base) = find("dicom/pydicom-test-files/dicomdirtests") else { return };
    for name in ["DICOMDIR", "DICOMDIR-implicit", "DICOMDIR-bigEnd", "DICOMDIR-reordered", "DICOMDIR-nooffset"] {
        let d = Dicom::open(&base.join(name)).unwrap();
        assert!(d.is_directory(), "{name}");
        let records = slerp_dicom::dicomdir::records(d.data(), &base);
        assert_eq!(slerp_dicom::dicomdir::count(&records, "PATIENT"), 2, "{name}");
        assert_eq!(slerp_dicom::dicomdir::count(&records, "IMAGE"), 31, "{name}");
        assert!(slerp_dicom::dicomdir::first_image(&records).is_some(), "{name}");
    }
    // A DICOMDIR's picture is its first image.
    assert!(slerp_dicom::picture_file(&base.join("DICOMDIR"), Some(64), true).is_ok());
    assert!(slerp_dicom::picture_file(&base.join("DICOMDIR"), Some(64), false).is_err());
}

//endregion

/*┌─────────────────╮
| NEMA and TCIA   |
└─────────────────╯*/
//region NEMA and TCIA

/// NEMA's compressed samples: every lossless codec equal to the
/// uncompressed reference, the lossy ones close.
#[test]
fn nema_compressed_samples() {
    let Some(root) = find("dicom-extra/nema-wg04/IMAGES") else { return };
    for image in
        ["CT1", "CT2", "MG1", "MR1", "MR2", "MR3", "MR4", "NM1", "RG1", "RG2", "RG3", "SC1", "XA1", "US1", "VL1"]
    {
        let reference = format!("dicom-extra/nema-wg04/IMAGES/REF/{image}_UNC");
        for codec in ["JPLL", "JLSL", "J2KR"] {
            let f = format!("dicom-extra/nema-wg04/IMAGES/{codec}/{image}_{codec}");
            if root.join(codec).join(format!("{image}_{codec}")).exists() {
                same(&[&reference, &f]);
            }
        }
        // NEMA's irreversible JPEG 2000 files are compressed up to 160:1.
        for (codec, limit) in [("JPLY", 1.0), ("JLSN", 1.0), ("J2KI", 3.0)] {
            if root.join(codec).join(format!("{image}_{codec}")).exists() {
                close(&reference, &format!("dicom-extra/nema-wg04/IMAGES/{codec}/{image}_{codec}"), limit);
            }
        }
    }
}

/// A CT series of extension-less files sorts by instance number, which
/// agrees with position along the slice normal.
#[test]
fn ct_series_order() {
    let Some(dir) = find("dicom/tcia-lidc-idri-0001/ct") else { return };
    let any =
        std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).find(|p| slerp_dicom::series::is_dicom_file(p));
    let Some(any) = any else { return };
    let t = std::time::Instant::now();
    let series = slerp_dicom::series::series_of(&any);
    eprintln!("{} slices sorted in {:?}", series.len(), t.elapsed());
    assert_eq!(series.len(), 133);
    let z: Vec<f64> =
        series.iter().map(|p| slerp_dicom::series::Slice::read(p).unwrap().position.unwrap()[2]).collect();
    let rising = z.windows(2).all(|w| w[1] > w[0]);
    let falling = z.windows(2).all(|w| w[1] < w[0]);
    assert!(rising || falling, "slices out of order: {z:?}");
    // The folder holds that one series.
    let all = slerp_dicom::series::folder_series(&dir);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].files, series);
    assert_eq!(all[0].modality, "CT");
}

/// A folder of several series (pydicom's DICOMDIR test set, one series
/// per folder there) and the DICOMDIR's own listing agree.
#[test]
fn series_per_folder() {
    let Some(dir) = find("dicom/pydicom-test-files/dicomdirtests/98892003/MR700") else { return };
    let all = slerp_dicom::series::folder_series(&dir);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].files.len(), 7);
    let numbers: Vec<i64> = all[0]
        .files
        .iter()
        .map(|p| slerp_dicom::Dicom::open(p).unwrap().data().int(slerp_dicom::tags::INSTANCE_NUMBER).unwrap())
        .collect();
    assert!(numbers.windows(2).all(|w| w[0] < w[1]), "{numbers:?}");
}

/// CT in Hounsfield units: air about -1000, and the stored window applies.
#[test]
fn ct_hounsfield_units() {
    let Some(dir) = find("dicom/tcia-lidc-idri-0001/ct") else { return };
    let Some(file) =
        std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).find(|p| slerp_dicom::series::is_dicom_file(p))
    else {
        return;
    };
    let d = Dicom::open(&file).unwrap();
    assert!(d.is_ct());
    let f = d.decode_frame(0, None).unwrap();
    let FramePixels::Grey(v) = &f.pixels else { panic!() };
    // The corner is outside the body: air, or padding below it.
    assert!(v[0] < -900.0, "{}", v[0]);
    let mut sorted: Vec<f32> = v.clone();
    sorted.sort_by(f32::total_cmp);
    let median = sorted[sorted.len() / 2];
    assert!((-1100.0..200.0).contains(&median), "{median}");
}

//endregion

/*┌─────────────────╮
| Damaged streams |
└─────────────────╯*/
//region Damaged streams

/// One small sample per codec; the damage tests below run over them.
const CODEC_SAMPLES: &[&str] = &[
    "dicom/pydicom-test-files/MR_small_RLE.dcm",
    "dicom/pydicom-test-files/MR_small_jpeg_ls_lossless.dcm",
    "dicom/pydicom-test-files/JPEGLSNearLossless_16.dcm",
    "dicom/pydicom-data/JLSL_RGB_ILV1.dcm",
    "dicom/pydicom-data/JLSL_RGB_ILV2.dcm",
    "dicom/pydicom-test-files/MR_small_jp2klossless.dcm",
    "dicom/pydicom-test-files/JPEG2000.dcm",
    "dicom/pydicom-test-files/GDCMJ2K_TextGBR.dcm",
    "dicom/pydicom-test-files/JPEG-lossy.dcm",
    "dicom/pydicom-test-files/SC_rgb_dcmtk_+eb+cy+n2.dcm",
    "dicom/pydicom-test-files/SC_rgb_jpeg_dcmtk.dcm",
    "dicom/pydicom-test-files/SC_rgb_jpeg_gdcm.dcm",
    "dicom/pydicom-data/emri_small_jpeg_2k_lossless.dcm",
];

/// Reads everything a viewer would from damaged bytes.
fn open_and_decode(bytes: Vec<u8>) {
    if let Ok(d) = Dicom::from_bytes(bytes) {
        for i in 0..d.frame_count().min(3) {
            if let Ok(f) = d.decode_frame(i, None) {
                let voi = d.default_voi(i, &f);
                let _ = d.picture(&f, &voi, d.inverted(), d.pixel_aspect(i));
            }
            let _ = d.decode_frame(i, Some(32));
        }
    }
}

#[test]
fn truncated_compressed_files_never_panic() {
    for rel in CODEC_SAMPLES {
        let Some(p) = find(rel) else { continue };
        let file = std::fs::read(&p).unwrap();
        // Every cut in the last part of the file (inside the pixel data),
        // and a stride of the rest.
        let step = (file.len() / 400).max(1);
        for len in (0..file.len()).step_by(step).chain(file.len().saturating_sub(300)..file.len()) {
            open_and_decode(file[..len].to_vec());
        }
    }
}

#[test]
fn mutated_compressed_files_never_panic() {
    let mut seed = 0x9E3779B97F4A7C15u64;
    let mut rand = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for rel in CODEC_SAMPLES {
        let Some(p) = find(rel) else { continue };
        let file = std::fs::read(&p).unwrap();
        // Damage lands in the pixel data (the last part) more often than in
        // the header, since the codecs are what is being tested.
        for _ in 0..300 {
            let mut f = file.clone();
            for _ in 0..1 + rand() % 6 {
                let tail = (file.len() as u64).min(8192);
                let i = if rand() % 4 == 0 { rand() % file.len() as u64 } else { file.len() as u64 - 1 - rand() % tail }
                    as usize;
                f[i] = match rand() % 3 {
                    0 => 0xFF,
                    1 => f[i] ^ (1 << (rand() % 8)),
                    _ => rand() as u8,
                };
            }
            open_and_decode(f);
        }
    }
}

/// Streams whose own headers claim enormous images are refused before
/// anything is allocated: run under a memory limit, this would die if not.
#[test]
fn absurd_stream_headers_are_refused() {
    // JPEG-LS: SOF55 with 65535 x 65535 and four components.
    let jls =
        [0xFF, 0xD8, 0xFF, 0xF7, 0, 14, 8, 0xFF, 0xFF, 0xFF, 0xFF, 3, 1, 0x11, 0, 2, 0x11, 0, 3, 0x11, 0, 0xFF, 0xD9];
    // JPEG: baseline SOF0 with 65535 x 65535, three components.
    let jpeg =
        [0xFF, 0xD8, 0xFF, 0xC0, 0, 17, 8, 0xFF, 0xFF, 0xFF, 0xFF, 3, 1, 0x11, 0, 2, 0x11, 1, 3, 0x11, 1, 0xFF, 0xD9];
    // 12-bit JPEG likewise.
    let jpeg12 = {
        let mut v = jpeg;
        v[6] = 12;
        v
    };
    // JPEG 2000: SIZ claiming 2^32-1 x 2^32-1.
    let mut j2k = vec![0xFF, 0x4F, 0xFF, 0x51, 0, 41, 0, 0];
    j2k.extend([0xFF; 8]); // Xsiz, Ysiz
    j2k.extend([0; 8]); // offsets
    j2k.extend([0, 0, 1, 0, 0, 0, 1, 0]); // tile size
    j2k.extend([0; 8]); // tile offsets
    j2k.extend([0, 1, 7, 1, 1]);
    j2k.extend([0xFF, 0xD9]);
    for (name, stream, uid) in [
        ("JPEG-LS", jls.to_vec(), "1.2.840.10008.1.2.4.80"),
        ("JPEG", jpeg.to_vec(), "1.2.840.10008.1.2.4.50"),
        ("12-bit JPEG", jpeg12.to_vec(), "1.2.840.10008.1.2.4.51"),
        ("JPEG 2000", j2k, "1.2.840.10008.1.2.4.90"),
    ] {
        let d = tiny_encapsulated(uid, stream);
        let r = d.decode_frame(0, None);
        assert!(r.is_err(), "{name} claimed a huge image and decoded");
    }
}

/// A file with one 4 x 4 encapsulated frame holding `stream`, in the
/// transfer syntax `uid`.
fn tiny_encapsulated(uid: &str, stream: Vec<u8>) -> Dicom {
    fn el(g: u16, e: u16, vr: &[u8; 2], v: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend(g.to_le_bytes());
        out.extend(e.to_le_bytes());
        out.extend(vr);
        let mut v = v.to_vec();
        if v.len() % 2 == 1 {
            v.push(0);
        }
        out.extend((v.len() as u16).to_le_bytes());
        out.extend(v);
        out
    }
    let mut meta = Vec::new();
    meta.extend(el(2, 0x10, b"UI", uid.as_bytes()));
    let mut f = vec![0u8; 128];
    f.extend(b"DICM");
    f.extend(el(2, 0, b"UL", &(meta.len() as u32).to_le_bytes()));
    f.extend(meta);
    f.extend(el(0x28, 2, b"US", &1u16.to_le_bytes()));
    f.extend(el(0x28, 4, b"CS", b"MONOCHROME2"));
    f.extend(el(0x28, 0x10, b"US", &4u16.to_le_bytes()));
    f.extend(el(0x28, 0x11, b"US", &4u16.to_le_bytes()));
    f.extend(el(0x28, 0x100, b"US", &16u16.to_le_bytes()));
    f.extend(el(0x28, 0x101, b"US", &16u16.to_le_bytes()));
    f.extend(el(0x28, 0x102, b"US", &15u16.to_le_bytes()));
    f.extend(el(0x28, 0x103, b"US", &0u16.to_le_bytes()));
    f.extend([0xE0, 0x7F, 0x10, 0x00, b'O', b'B', 0, 0, 0xFF, 0xFF, 0xFF, 0xFF]);
    f.extend([0xFE, 0xFF, 0x00, 0xE0, 0, 0, 0, 0]);
    let mut stream = stream;
    if stream.len() % 2 == 1 {
        stream.push(0);
    }
    f.extend([0xFE, 0xFF, 0x00, 0xE0]);
    f.extend((stream.len() as u32).to_le_bytes());
    f.extend(stream);
    f.extend([0xFE, 0xFF, 0xDD, 0xE0, 0, 0, 0, 0]);
    Dicom::from_bytes(f).expect("the container is valid")
}

//endregion
