//! The array model every format in this crate reads into, and the plane
//! extraction the thumbnails and the viewer share.
//!
//! A [`Volume`] is an n-dimensional array of numbers that stays where it is:
//! in the file, in a compressed stream, or in memory.  Its axes say how to
//! step through it (a stride in samples each), what each means (a column, a
//! row, a slice, a time point, a colour channel) and, for medical data, which
//! way it points in the body.  A plane is one picture cut from it: an axial,
//! coronal or sagittal slice of one time point, read with only the bytes it
//! needs.

use crate::dtype::{DType, Endian, decode};
use crate::source::{ByteSource, MemSource};
use std::io;
use std::sync::Arc;

/// What an axis is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    /// Columns of an axial picture.
    X,
    /// Rows of an axial picture.
    Y,
    /// The slice axis of an axial picture.
    Z,
    /// Time points (or diffusion directions): shown one volume at a time.
    T,
    /// The samples of a pixel, red green blue (alpha).
    Color,
    /// Channels of a microscope image, each with its own name and colour.
    Channel,
    /// Anything else (a fifth dimension): held at its first index.
    Other,
}

/// The direction in the body an axis points to as its index grows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    R,
    L,
    A,
    P,
    S,
    I,
}

impl Dir {
    pub fn letter(self) -> char {
        match self {
            Dir::R => 'R',
            Dir::L => 'L',
            Dir::A => 'A',
            Dir::P => 'P',
            Dir::S => 'S',
            Dir::I => 'I',
        }
    }

    pub fn opposite(self) -> Dir {
        match self {
            Dir::R => Dir::L,
            Dir::L => Dir::R,
            Dir::A => Dir::P,
            Dir::P => Dir::A,
            Dir::S => Dir::I,
            Dir::I => Dir::S,
        }
    }

    /// The role an axis pointing this way plays in a body-aligned volume.
    pub fn role(self) -> Role {
        match self {
            Dir::R | Dir::L => Role::X,
            Dir::A | Dir::P => Role::Y,
            Dir::S | Dir::I => Role::Z,
        }
    }
}

/// One dimension of the stored array.
#[derive(Clone, Debug)]
pub struct Axis {
    pub len: usize,
    /// Distance in samples between neighbours along this axis.
    pub stride: usize,
    pub role: Role,
    /// Size of one step along the axis in the volume's unit; 0 when the
    /// file doesn't say.
    pub spacing: f64,
    pub name: String,
    /// The array is stored backwards along this axis (NetCDF grids with
    /// latitudes counting up); pictures put the first index last.
    pub reverse: bool,
    pub dir: Option<Dir>,
}

impl Axis {
    pub fn new(len: usize, stride: usize, role: Role) -> Axis {
        Axis { len, stride, role, spacing: 0.0, name: String::new(), reverse: false, dir: None }
    }

    pub fn named(mut self, name: impl Into<String>) -> Axis {
        self.name = name.into();
        self
    }

    pub fn with_spacing(mut self, spacing: f64) -> Axis {
        if spacing.is_finite() && spacing > 0.0 {
            self.spacing = spacing;
        }
        self
    }
}

/// A microscope channel.
#[derive(Clone, Debug, PartialEq)]
pub struct ChannelInfo {
    pub name: String,
    pub color: [u8; 3],
}

/// The array and everything the file says about it.
#[derive(Clone)]
pub struct Volume {
    /// What the array is called inside its file (an `.npz` member, an HDF5
    /// dataset), or the file's own name.
    pub name: String,
    /// "NumPy .npy 1.0", "NIfTI-1", "OME-TIFF"...
    pub format: String,
    pub axes: Vec<Axis>,
    pub dtype: DType,
    pub endian: Endian,
    pub source: Arc<dyn ByteSource>,
    /// Stored values become `raw * slope + inter` (NIfTI's `scl_slope`).
    pub slope: f64,
    pub inter: f64,
    /// The unit of spacings ("mm", "um").
    pub unit: String,
    /// What the values mean ("HU", "degC"), when the file says.
    pub value_name: String,
    /// Low values are bright (TIFF's WhiteIsZero).
    pub invert: bool,
    /// The range the file asks to display (NIfTI `cal_min`/`cal_max`).
    pub display_range: Option<(f64, f64)>,
    /// Stored values equal to this mean "no data" (NetCDF's `_FillValue`) and
    /// are left out of pictures and statistics.
    pub fill: Option<f64>,
    /// One entry for each index of the `Channel` axis.
    pub channels: Vec<ChannelInfo>,
    /// Header fields for the inspector, in file order.
    pub facts: Vec<(String, String)>,
    pub warnings: Vec<String>,
    /// Which axis is the slice axis is a guess, so the viewer offers a switch.
    pub slice_axis_guess: bool,
    /// Why no picture can be made (a one-dimensional array); the viewer
    /// shows statistics instead.
    pub no_picture: Option<String>,
}

impl std::fmt::Debug for Volume {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Volume({} {:?} {:?})", self.format, self.dtype, self.shape())
    }
}

/// Which axes make the picture, the slices and the time points.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AxisMap {
    pub x: usize,
    pub y: usize,
    pub z: Option<usize>,
    pub t: Option<usize>,
    /// The `Color` or `Channel` axis.
    pub color: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PlaneKind {
    #[default]
    Axial,
    Coronal,
    Sagittal,
}

impl PlaneKind {
    pub const ALL: [PlaneKind; 3] = [PlaneKind::Axial, PlaneKind::Coronal, PlaneKind::Sagittal];

    pub fn name(self) -> &'static str {
        match self {
            PlaneKind::Axial => "Axial",
            PlaneKind::Coronal => "Coronal",
            PlaneKind::Sagittal => "Sagittal",
        }
    }
}

/// Which plane to cut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SliceView {
    pub kind: PlaneKind,
    /// The slice, counted in the order pictures show (towards the head,
    /// the front, the left).
    pub slice: usize,
    pub t: usize,
    /// Show the patient's left on the left of the picture instead of the
    /// right.
    pub neurological: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelSel {
    /// Every sample of the colour or channel axis.
    All,
    One(usize),
}

/// One picture cut from a volume.
#[derive(Clone, Debug)]
pub struct Slice {
    pub width: usize,
    pub height: usize,
    /// Values per pixel: 1, or the colour or channel axis' length.
    pub samples: usize,
    /// Rows from the top, pixels left to right, samples interleaved.
    pub data: Vec<f32>,
    /// Physical size of a pixel, 0 when unknown.
    pub col_spacing: f64,
    pub row_spacing: f64,
    /// How many stored pixels each picture pixel covers.
    pub step: usize,
    /// Body directions at the left, right, top and bottom edges.
    pub edges: [Option<char>; 4],
    /// The axes whose indices vary along the picture's columns and rows.
    pub col_axis: usize,
    pub row_axis: usize,
    /// The stored index along every other axis, with 0 at the row and column
    /// axes.
    pub fixed: Vec<usize>,
    /// The picture is mirrored from the stored order, and the stored sizes it
    /// was cut from.
    pub flip_rows: bool,
    pub flip_cols: bool,
    pub rows: usize,
    pub cols: usize,
    /// The axis a maximum intensity projection ran along, whose index then
    /// means nothing.
    pub projected_axis: Option<usize>,
}

impl Slice {
    /// The array indices (along every axis, in the volume's axis order) the
    /// picture pixel (x, y) came from.
    pub fn voxel(&self, x: usize, y: usize) -> Vec<usize> {
        let (x, y) = (x.min(self.width.saturating_sub(1)), y.min(self.height.saturating_sub(1)));
        let x = if self.flip_cols { self.width - 1 - x } else { x };
        let y = if self.flip_rows { self.height - 1 - y } else { y };
        let mut idx = self.fixed.clone();
        idx[self.col_axis] = (x * self.step).min(self.cols.saturating_sub(1));
        idx[self.row_axis] = (y * self.step).min(self.rows.saturating_sub(1));
        idx
    }

    pub fn value(&self, x: usize, y: usize, sample: usize) -> Option<f32> {
        (x < self.width && y < self.height && sample < self.samples)
            .then(|| self.data[(y * self.width + x) * self.samples + sample])
    }
}

/// A cap on the pixels of one picture, so a damaged header can't ask for
/// gigabytes.
pub const MAX_PLANE_SAMPLES: usize = 1 << 28;

impl Volume {
    /// An array with no spacing, orientation or extras.
    pub fn new(
        name: impl Into<String>,
        format: impl Into<String>,
        axes: Vec<Axis>,
        dtype: DType,
        endian: Endian,
        source: Arc<dyn ByteSource>,
    ) -> Volume {
        Volume {
            name: name.into(),
            format: format.into(),
            axes,
            dtype,
            endian,
            source,
            slope: 1.0,
            inter: 0.0,
            unit: String::new(),
            value_name: String::new(),
            invert: false,
            display_range: None,
            fill: None,
            channels: Vec::new(),
            facts: Vec::new(),
            warnings: Vec::new(),
            slice_axis_guess: false,
            no_picture: None,
        }
    }

    pub fn shape(&self) -> Vec<usize> {
        self.axes.iter().map(|a| a.len).collect()
    }

    /// The number of samples in the array.
    pub fn samples(&self) -> u64 {
        self.axes.iter().fold(1u64, |n, a| n.saturating_mul(a.len as u64))
    }

    /// The bytes the array covers.
    pub fn bytes(&self) -> u64 {
        self.samples().saturating_mul(self.dtype.size() as u64)
    }

    pub fn axis_with(&self, role: Role) -> Option<usize> {
        self.axes.iter().position(|a| a.role == role)
    }

    pub fn fact(&mut self, key: &str, value: impl Into<String>) {
        let v = value.into();
        if !v.is_empty() {
            self.facts.push((key.to_string(), v));
        }
    }

    /// Checks the axes against the data, clipping the slowest axis when the
    /// data ends early (a cut-off file still shows the slices that arrived).
    /// An error for absurd sizes and for files with no complete slice.
    pub fn fit(&mut self) -> Result<(), String> {
        if self.axes.is_empty() {
            return Err("the array has no dimensions".into());
        }
        let mut total = 1u64;
        for a in &self.axes {
            if a.len == 0 {
                return Err("the array is empty".into());
            }
            total = total.checked_mul(a.len as u64).ok_or("the array's size is absurd")?;
        }
        let esize = self.dtype.size() as u64;
        // The element past the last one reached, in samples.
        let mut reach = 1u64;
        for a in &self.axes {
            let span = (a.len as u64 - 1).checked_mul(a.stride as u64).ok_or("the array's size is absurd")?;
            reach = reach.checked_add(span).ok_or("the array's size is absurd")?;
        }
        let need = reach.checked_mul(esize).ok_or("the array's size is absurd")?;
        let have = self.source.len();
        if need <= have {
            return Ok(());
        }
        // Clip the axis with the largest stride to the slices that are all there.
        let (slow, stride) = self
            .axes
            .iter()
            .enumerate()
            .max_by_key(|(_, a)| a.stride)
            .map(|(i, a)| (i, a.stride as u64))
            .ok_or("the array has no dimensions")?;
        let other: u64 = self
            .axes
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != slow)
            .map(|(_, a)| (a.len as u64 - 1) * a.stride as u64)
            .sum();
        let avail = have / esize;
        let fit = if stride == 0 || avail <= other { 0 } else { (avail - other - 1) / stride + 1 };
        if fit == 0 {
            return Err("the file ends before the first slice is complete".into());
        }
        let was = self.axes[slow].len;
        self.axes[slow].len = (fit as usize).min(was);
        self.warnings.push(format!(
            "The file ends early: {} of {} along the {} axis are there.",
            self.axes[slow].len,
            was,
            if self.axes[slow].name.is_empty() { "slowest" } else { &self.axes[slow].name }
        ));
        Ok(())
    }

    /// Copies the data into memory when it takes at most `limit` bytes, so
    /// every plane after the first costs nothing.  Reads are by block.
    pub fn into_memory(mut self, limit: u64) -> io::Result<Volume> {
        if self.source.as_slice().is_some() {
            return Ok(self);
        }
        let need = (self.bytes()).min(self.source.len());
        if need > limit {
            return Ok(self);
        }
        let mut data = Vec::new();
        data.try_reserve_exact(need as usize).map_err(|_| io::Error::other("out of memory"))?;
        let mut chunk = vec![0u8; 1 << 20];
        let mut at = 0u64;
        while at < need {
            let n = ((need - at) as usize).min(chunk.len());
            self.source.read_at(at, &mut chunk[..n])?;
            data.extend_from_slice(&chunk[..n]);
            at += n as u64;
        }
        self.source = Arc::new(MemSource(data));
        Ok(self)
    }

    /*┌─────────────────╮
    | Axes            |
    └─────────────────╯*/
    //region Axes

    /// The axes as the file's roles say.
    pub fn default_map(&self) -> Option<AxisMap> {
        let x = self.axis_with(Role::X)?;
        let y = self.axis_with(Role::Y)?;
        Some(AxisMap {
            x,
            y,
            z: self.axis_with(Role::Z),
            t: self.axis_with(Role::T),
            color: self.axis_with(Role::Color).or_else(|| self.axis_with(Role::Channel)),
        })
    }

    /// Every way to choose the slice axis of a volume whose orientation the
    /// file doesn't give: the chosen axis becomes Z, and the other two keep
    /// their order as rows and columns.  Empty for oriented and flat data.
    pub fn slice_axis_choices(&self) -> Vec<(String, AxisMap)> {
        let Some(base) = self.default_map() else { return Vec::new() };
        let Some(z) = base.z else { return Vec::new() };
        if !self.slice_axis_guess {
            return Vec::new();
        }
        let mut spatial = [base.z.unwrap_or(z), base.y, base.x];
        spatial.sort_unstable();
        spatial
            .iter()
            .map(|&s| {
                let rest: Vec<usize> = spatial.iter().copied().filter(|&a| a != s).collect();
                (format!("Axis {s}"), AxisMap { x: rest[1], y: rest[0], z: Some(s), ..base })
            })
            .collect()
    }

    /// The axis a plane kind steps through.
    pub fn depth_axis(&self, map: &AxisMap, kind: PlaneKind) -> Option<usize> {
        match kind {
            PlaneKind::Axial => map.z,
            PlaneKind::Coronal => Some(map.y),
            PlaneKind::Sagittal => Some(map.x),
        }
    }

    /// How many slices a plane kind has.
    pub fn slice_count(&self, map: &AxisMap, kind: PlaneKind) -> usize {
        match kind {
            PlaneKind::Axial => map.z.map_or(1, |z| self.axes[z].len),
            _ => self.depth_axis(map, kind).map_or(1, |a| self.axes[a].len),
        }
    }

    pub fn time_count(&self, map: &AxisMap) -> usize {
        map.t.map_or(1, |t| self.axes[t].len)
    }

    /// Whether the body-direction flags apply: all three spatial axes are
    /// labelled.
    pub fn oriented(&self, map: &AxisMap) -> bool {
        let dir = |a: usize| self.axes[a].dir;
        dir(map.x).is_some() && dir(map.y).is_some() && map.z.is_some_and(|z| dir(z).is_some())
    }

    /// Rows, columns and slices of a plane kind, and which ways the picture
    /// is mirrored: (row axis, column axis, depth axis, reverse slices,
    /// flip rows, flip columns, edge letters).
    #[allow(clippy::type_complexity)]
    fn geometry(
        &self,
        map: &AxisMap,
        kind: PlaneKind,
        neurological: bool,
    ) -> (usize, usize, Option<usize>, bool, bool, bool, [Option<char>; 4]) {
        let (rows, cols, depth) = match kind {
            PlaneKind::Axial => (map.y, map.x, map.z),
            PlaneKind::Coronal => (map.z.unwrap_or(map.y), map.x, Some(map.y)),
            PlaneKind::Sagittal => (map.z.unwrap_or(map.y), map.y, Some(map.x)),
        };
        let dir = |a: usize| self.axes[a].dir;
        let reversed = |a: usize| self.axes[a].reverse;
        // Axes the file stores backwards are mirrored first.
        let mut rev_depth = depth.is_some_and(reversed);
        let mut flip_rows = reversed(rows);
        let mut flip_cols = reversed(cols);
        let mut edges = [None; 4];
        if self.oriented(map) {
            // Slices count towards the head, the front and the left; pictures
            // show the head up, the front up (axial) and, in the radiological
            // style, the patient's left on the right.
            let want_depth = match kind {
                PlaneKind::Axial => Dir::S,
                PlaneKind::Coronal => Dir::A,
                PlaneKind::Sagittal => Dir::L,
            };
            let (want_rows, want_cols) = match kind {
                // Rows grow downwards, so they should grow towards the back.
                PlaneKind::Axial => (Dir::P, Dir::L),
                PlaneKind::Coronal => (Dir::I, Dir::L),
                PlaneKind::Sagittal => (Dir::I, Dir::P),
            };
            let against = |a: Option<usize>, want: Dir| a.and_then(dir) == Some(want.opposite());
            rev_depth ^= against(depth, want_depth);
            flip_rows ^= against(Some(rows), want_rows);
            flip_cols ^= against(Some(cols), want_cols);
            let mut right = want_cols;
            if neurological && kind != PlaneKind::Sagittal {
                flip_cols = !flip_cols;
                right = want_cols.opposite();
            }
            edges = [
                Some(right.opposite().letter()),
                Some(right.letter()),
                Some(want_rows.opposite().letter()),
                Some(want_rows.letter()),
            ];
        }
        (rows, cols, depth, rev_depth, flip_rows, flip_cols, edges)
    }

    //endregion

    /*┌─────────────────╮
    | Planes          |
    └─────────────────╯*/
    //region Planes

    /// Cuts a plane.  A picture longer than `max_side` on its longer side is
    /// averaged down by a whole factor while it is read, so thumbnails of
    /// huge planes never hold them in memory.
    pub fn extract(
        &self,
        map: &AxisMap,
        view: &SliceView,
        channel: ChannelSel,
        max_side: Option<usize>,
    ) -> io::Result<Slice> {
        let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidData, m.to_string());
        let (row_axis, col_axis, depth_axis, rev_depth, flip_rows, flip_cols, edges) =
            self.geometry(map, view.kind, view.neurological);
        let rows = self.axes[row_axis].len;
        let cols = self.axes[col_axis].len;
        // 1. Fixed indices: the slice, the time point, and zero elsewhere.
        let at = |axis: usize, index: usize| index.min(self.axes[axis].len - 1) as u64 * self.axes[axis].stride as u64;
        let mut base = 0u64;
        if let Some(d) = depth_axis {
            let n = self.axes[d].len;
            let s = view.slice.min(n - 1);
            base += at(d, if rev_depth { n - 1 - s } else { s });
        }
        if let Some(t) = map.t {
            base += at(t, view.t);
        }
        for i in 0..self.axes.len() {
            let used = [Some(row_axis), Some(col_axis), depth_axis, map.t, map.color].contains(&Some(i));
            if !used {
                base += at(i, 0);
            }
        }
        // 2. Samples: all of the colour axis, or one.
        let (samples, sample_stride) = match (map.color, channel) {
            (Some(c), ChannelSel::All) => (self.axes[c].len, self.axes[c].stride as u64),
            (Some(c), ChannelSel::One(i)) => {
                base += at(c, i);
                (1, 1)
            }
            (None, _) => (1, 1),
        };
        // 3. How much to average away.
        let step = max_side.map_or(1, |m| rows.max(cols).div_ceil(m.max(1)).max(1));
        let (out_w, out_h) = (cols.div_ceil(step), rows.div_ceil(step));
        let total = out_w
            .checked_mul(out_h)
            .and_then(|n| n.checked_mul(samples))
            .filter(|&n| n <= MAX_PLANE_SAMPLES)
            .ok_or_else(|| bad("the picture is too large"))?;
        let mut data = Vec::new();
        data.try_reserve_exact(total).map_err(|_| io::Error::other("out of memory"))?;
        data.resize(total, 0.0f32);
        // 4. Read the rows, `step` at a time.
        let col_stride = self.axes[col_axis].stride as u64;
        let row_stride = self.axes[row_axis].stride as u64;
        let esize = self.dtype.size();
        let mut reader = Reader::new(&*self.source);
        // A row is one run of bytes when its pixels follow each other.
        let contiguous = col_stride == samples as u64 && (samples == 1 || sample_stride == 1);
        let mut row_bytes = vec![0u8; cols * samples * esize];
        let mut row_vals = vec![0f32; cols * samples];
        let mut sum = vec![0f32; out_w * samples];
        let mut count = vec![0u32; out_w * samples];
        for oy in 0..out_h {
            let (y0, y1) = (oy * step, (oy * step + step).min(rows));
            sum.fill(0.0);
            count.fill(0);
            for y in y0..y1 {
                let row_base = base + y as u64 * row_stride;
                if contiguous {
                    reader.read(row_base * esize as u64, &mut row_bytes)?;
                } else {
                    for x in 0..cols {
                        for s in 0..samples {
                            let el = row_base + x as u64 * col_stride + s as u64 * sample_stride;
                            let o = (x * samples + s) * esize;
                            reader.read(el * esize as u64, &mut row_bytes[o..o + esize])?;
                        }
                    }
                }
                decode(self.dtype, self.endian, &row_bytes, &mut row_vals);
                if let Some(f) = self.fill {
                    let f = f as f32;
                    for v in &mut row_vals {
                        if *v == f {
                            *v = f32::NAN;
                        }
                    }
                }
                if step == 1 {
                    data[oy * out_w * samples..(oy + 1) * out_w * samples].copy_from_slice(&row_vals);
                } else {
                    // Averaging skips non-finite values so one NaN doesn't blank a block.
                    for x in 0..cols {
                        for s in 0..samples {
                            let v = row_vals[x * samples + s];
                            if v.is_finite() {
                                sum[(x / step) * samples + s] += v;
                                count[(x / step) * samples + s] += 1;
                            }
                        }
                    }
                }
            }
            if step > 1 {
                let out_row = &mut data[oy * out_w * samples..(oy + 1) * out_w * samples];
                for (i, o) in out_row.iter_mut().enumerate() {
                    *o = if count[i] > 0 { sum[i] / count[i] as f32 } else { f32::NAN };
                }
            }
        }
        // 5. Scale, mirror and report.
        if self.slope != 1.0 || self.inter != 0.0 {
            let (m, b) = (self.slope as f32, self.inter as f32);
            for v in &mut data {
                *v = *v * m + b;
            }
        }
        let mut fixed = vec![0usize; self.axes.len()];
        let mut stored = |axis: usize, index: usize, rev: bool| {
            let n = self.axes[axis].len;
            let i = index.min(n - 1);
            fixed[axis] = if rev { n - 1 - i } else { i };
        };
        if let Some(d) = depth_axis {
            stored(d, view.slice, rev_depth);
        }
        if let Some(tt) = map.t {
            stored(tt, view.t, false);
        }
        if let (Some(c), ChannelSel::One(i)) = (map.color, channel) {
            stored(c, i, false);
        }
        let mut slice = Slice {
            width: out_w,
            height: out_h,
            samples,
            data,
            col_spacing: self.axes[col_axis].spacing * step as f64,
            row_spacing: self.axes[row_axis].spacing * step as f64,
            step,
            edges,
            col_axis,
            row_axis,
            fixed,
            flip_rows,
            flip_cols,
            rows,
            cols,
            projected_axis: None,
        };
        if flip_cols {
            flip_horizontally(&mut slice);
        }
        if flip_rows {
            flip_vertically(&mut slice);
        }
        Ok(slice)
    }

    /// The value at one position, indexed along every axis in `self.axes`
    /// order, with the scale applied and full precision for 64-bit types.
    pub fn value_at(&self, index: &[usize]) -> io::Result<f64> {
        if index.len() != self.axes.len() {
            return Err(io::Error::other("the index has the wrong length"));
        }
        let mut at = 0u64;
        for (a, &i) in self.axes.iter().zip(index) {
            if i >= a.len {
                return Err(io::Error::other("the index is outside the array"));
            }
            at += i as u64 * a.stride as u64;
        }
        let mut buf = [0u8; 16];
        let n = self.dtype.size();
        self.source.read_at(at * n as u64, &mut buf[..n])?;
        let raw = crate::dtype::decode_f64(self.dtype, self.endian, &buf[..n]);
        if self.fill == Some(raw) {
            return Ok(f64::NAN);
        }
        Ok(raw * self.slope + self.inter)
    }

    /// `count` consecutive samples from flat sample number `start` (counting in
    /// storage order), as `f32`, with the fill value left out and the scale
    /// applied.
    pub fn read_values(&self, start: u64, count: usize) -> io::Result<Vec<f32>> {
        let esize = self.dtype.size() as u64;
        let have = self.source.len() / esize;
        let n = (count as u64).min(have.saturating_sub(start)) as usize;
        let mut bytes = vec![0u8; n * esize as usize];
        self.source.read_at(start * esize, &mut bytes)?;
        let mut out = vec![0f32; n];
        decode(self.dtype, self.endian, &bytes, &mut out);
        for v in &mut out {
            if self.fill.is_some_and(|f| *v == f as f32) {
                *v = f32::NAN;
            } else {
                *v = *v * self.slope as f32 + self.inter as f32;
            }
        }
        Ok(out)
    }

    /// The brightest value along the slice axis for every pixel, a maximum
    /// intensity projection.  Reads the whole volume, so callers keep it to
    /// small ones.
    pub fn mip(&self, map: &AxisMap, view: &SliceView, channel: ChannelSel) -> io::Result<Slice> {
        let n = self.slice_count(map, view.kind);
        let mut best: Option<Slice> = None;
        for i in 0..n {
            let s = self.extract(map, &SliceView { slice: i, ..*view }, channel, None)?;
            match &mut best {
                None => best = Some(s),
                Some(b) => {
                    for (m, v) in b.data.iter_mut().zip(&s.data) {
                        if v.is_finite() && (!m.is_finite() || *v > *m) {
                            *m = *v;
                        }
                    }
                }
            }
        }
        let mut best = best.ok_or_else(|| io::Error::other("no slices"))?;
        best.projected_axis = self.depth_axis(map, view.kind);
        Ok(best)
    }

    /// Sampled statistics of the whole array, from evenly spread stretches.
    pub fn stats(&self) -> io::Result<Stats> {
        const CHUNK: u64 = 16384;
        const CHUNKS: u64 = 64;
        let total = self.samples();
        let esize = self.dtype.size() as u64;
        // The flat array may be shorter than `samples` x stride suggests; use
        // the samples before the data ends.
        let have = (self.source.len() / esize).min(total.max(1));
        let mut values: Vec<f32> = Vec::new();
        let mut reader = Reader::new(&*self.source);
        let mut bytes = vec![0u8; (CHUNK * esize) as usize];
        let mut tmp = vec![0f32; CHUNK as usize];
        let (chunks, spread) = if have <= CHUNK * CHUNKS { (have.div_ceil(CHUNK), false) } else { (CHUNKS, true) };
        for c in 0..chunks {
            // Small arrays are read whole; large ones from stretches spread evenly.
            let start = if spread { c * (have - CHUNK) / (chunks - 1) } else { c * CHUNK };
            let n = CHUNK.min(have - start);
            reader.read(start * esize, &mut bytes[..(n * esize) as usize])?;
            decode(self.dtype, self.endian, &bytes[..(n * esize) as usize], &mut tmp[..n as usize]);
            values.extend_from_slice(&tmp[..n as usize]);
        }
        if let Some(f) = self.fill {
            let f = f as f32;
            for v in &mut values {
                if *v == f {
                    *v = f32::NAN;
                }
            }
        }
        if self.slope != 1.0 || self.inter != 0.0 {
            for v in &mut values {
                *v = *v * self.slope as f32 + self.inter as f32;
            }
        }
        Ok(Stats::of(&mut values, total))
    }

    //endregion
}

fn flip_horizontally(s: &mut Slice) {
    let (w, n) = (s.width, s.samples);
    for row in s.data.chunks_exact_mut(w * n) {
        for x in 0..w / 2 {
            for k in 0..n {
                row.swap(x * n + k, (w - 1 - x) * n + k);
            }
        }
    }
}

fn flip_vertically(s: &mut Slice) {
    let rowlen = s.width * s.samples;
    let h = s.height;
    for y in 0..h / 2 {
        let (a, b) = s.data.split_at_mut((h - 1 - y) * rowlen);
        a[y * rowlen..(y + 1) * rowlen].swap_with_slice(&mut b[..rowlen]);
    }
}

/// Reads with a small cache, so gathers that touch one sample after another
/// across a file don't make a system call each.
pub struct Reader<'a> {
    src: &'a dyn ByteSource,
    mem: Option<&'a [u8]>,
    block: Vec<u8>,
    start: u64,
}

const BLOCK: usize = 1 << 20;

impl<'a> Reader<'a> {
    pub fn new(src: &'a dyn ByteSource) -> Reader<'a> {
        Reader { src, mem: src.as_slice(), block: Vec::new(), start: 0 }
    }

    pub fn read(&mut self, off: u64, buf: &mut [u8]) -> io::Result<()> {
        let end = off.checked_add(buf.len() as u64).ok_or_else(crate::source::eof)?;
        if let Some(m) = self.mem {
            let (s, e) = (usize::try_from(off).map_err(|_| crate::source::eof())?, end as usize);
            buf.copy_from_slice(m.get(s..e).ok_or_else(crate::source::eof)?);
            return Ok(());
        }
        // Big reads go straight through.
        if buf.len() >= BLOCK / 4 {
            return self.src.read_at(off, buf);
        }
        let inside = off >= self.start && end <= self.start + self.block.len() as u64;
        if !inside {
            let n = (BLOCK as u64).min(self.src.len().saturating_sub(off)) as usize;
            if n < buf.len() {
                return Err(crate::source::eof());
            }
            self.block.resize(n, 0);
            self.src.read_at(off, &mut self.block)?;
            self.start = off;
        }
        let s = (off - self.start) as usize;
        buf.copy_from_slice(&self.block[s..s + buf.len()]);
        Ok(())
    }
}

/*┌─────────────────╮
| Statistics      |
└─────────────────╯*/
//region Statistics

/// What a sample of the array looks like.
#[derive(Clone, Debug, Default)]
pub struct Stats {
    /// Samples looked at, out of `total`.
    pub sampled: u64,
    pub total: u64,
    pub min: f64,
    pub max: f64,
    pub mean: f64,
    pub std: f64,
    pub p1: f64,
    pub median: f64,
    pub p99: f64,
    pub nan: u64,
    pub inf: u64,
    pub zeros: u64,
    /// Counts for 64 equal bins from `min` to `max`.
    pub histogram: Vec<u32>,
}

impl Stats {
    /// Computes the statistics of `values`, reordering them.
    pub fn of(values: &mut Vec<f32>, total: u64) -> Stats {
        let mut st = Stats { sampled: values.len() as u64, total, ..Default::default() };
        values.retain(|v| {
            if v.is_nan() {
                st.nan += 1;
                false
            } else if v.is_infinite() {
                st.inf += 1;
                false
            } else {
                true
            }
        });
        if values.is_empty() {
            return st;
        }
        let n = values.len() as f64;
        let (mut sum, mut lo, mut hi) = (0f64, f64::MAX, f64::MIN);
        for &v in values.iter() {
            let v = v as f64;
            sum += v;
            lo = lo.min(v);
            hi = hi.max(v);
            if v == 0.0 {
                st.zeros += 1;
            }
        }
        st.mean = sum / n;
        st.std = (values.iter().map(|&v| (v as f64 - st.mean).powi(2)).sum::<f64>() / n).sqrt();
        st.min = lo;
        st.max = hi;
        st.histogram = vec![0; 64];
        let span = hi - lo;
        for &v in values.iter() {
            let b = if span > 0.0 { (((v as f64 - lo) / span) * 64.0) as usize } else { 0 };
            st.histogram[b.min(63)] += 1;
        }
        let pick = |values: &mut Vec<f32>, q: f64| -> f64 {
            let i = ((values.len() - 1) as f64 * q).round() as usize;
            *values.select_nth_unstable_by(i, |a, b| a.total_cmp(b)).1 as f64
        };
        st.p1 = pick(values, 0.01);
        st.median = pick(values, 0.5);
        st.p99 = pick(values, 0.99);
        st
    }
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    /// A 4 x 3 x 2 f32 array holding its own index: value = x + 10 y + 100 z.
    pub(crate) fn ramp() -> Volume {
        let (nx, ny, nz) = (4usize, 3usize, 2usize);
        let mut bytes = Vec::new();
        for z in 0..nz {
            for y in 0..ny {
                for x in 0..nx {
                    bytes.extend_from_slice(&((x + 10 * y + 100 * z) as f32).to_le_bytes());
                }
            }
        }
        let axes = vec![Axis::new(nz, nx * ny, Role::Z), Axis::new(ny, nx, Role::Y), Axis::new(nx, 1, Role::X)];
        Volume::new("ramp", "test", axes, DType::F32, Endian::Little, Arc::new(MemSource(bytes)))
    }

    fn view(kind: PlaneKind, slice: usize) -> SliceView {
        SliceView { kind, slice, t: 0, neurological: false }
    }

    #[test]
    fn axial_coronal_and_sagittal_planes_come_from_the_same_array() {
        let v = ramp();
        let m = v.default_map().unwrap();
        let ax = v.extract(&m, &view(PlaneKind::Axial, 1), ChannelSel::All, None).unwrap();
        assert_eq!((ax.width, ax.height), (4, 3));
        assert_eq!(ax.value(3, 2, 0), Some(123.0));
        let co = v.extract(&m, &view(PlaneKind::Coronal, 2), ChannelSel::All, None).unwrap();
        assert_eq!((co.width, co.height), (4, 2));
        assert_eq!(co.value(1, 1, 0), Some(121.0));
        let sa = v.extract(&m, &view(PlaneKind::Sagittal, 3), ChannelSel::All, None).unwrap();
        assert_eq!((sa.width, sa.height), (3, 2));
        assert_eq!(sa.value(2, 1, 0), Some(123.0));
        assert_eq!(v.slice_count(&m, PlaneKind::Axial), 2);
        assert_eq!(v.slice_count(&m, PlaneKind::Coronal), 3);
        assert_eq!(v.slice_count(&m, PlaneKind::Sagittal), 4);
    }

    #[test]
    fn slope_and_intercept_apply() {
        let mut v = ramp();
        v.slope = 2.0;
        v.inter = -1.0;
        let m = v.default_map().unwrap();
        let s = v.extract(&m, &view(PlaneKind::Axial, 0), ChannelSel::All, None).unwrap();
        assert_eq!(s.value(1, 1, 0), Some(2.0 * 11.0 - 1.0));
    }

    #[test]
    fn orientation_flips_follow_the_directions() {
        // x grows to the patient's right, y to the front, z to the head.
        let mut v = ramp();
        v.axes[2].dir = Some(Dir::R);
        v.axes[1].dir = Some(Dir::A);
        v.axes[0].dir = Some(Dir::S);
        let m = v.default_map().unwrap();
        assert!(v.oriented(&m));
        let ax = v.extract(&m, &view(PlaneKind::Axial, 0), ChannelSel::All, None).unwrap();
        // The front is up, so rows are reversed; the patient's left is on the
        // right, so columns are reversed too.
        assert_eq!(ax.value(0, 0, 0), Some(23.0));
        assert_eq!(ax.edges, [Some('R'), Some('L'), Some('A'), Some('P')]);
        // Slices count towards the head: the axis already does.
        let up = v.extract(&m, &view(PlaneKind::Axial, 1), ChannelSel::All, None).unwrap();
        assert_eq!(up.value(0, 0, 0), Some(123.0));
        // The same data stored towards the feet gives the same pictures.
        let mut w = ramp();
        w.axes[2].dir = Some(Dir::L);
        w.axes[1].dir = Some(Dir::P);
        w.axes[0].dir = Some(Dir::I);
        let wm = w.default_map().unwrap();
        let ax2 = w.extract(&wm, &view(PlaneKind::Axial, 1), ChannelSel::All, None).unwrap();
        assert_eq!(ax2.value(0, 0, 0), Some(0.0));
        let neuro = SliceView { neurological: true, ..view(PlaneKind::Axial, 0) };
        let n = v.extract(&m, &neuro, ChannelSel::All, None).unwrap();
        assert_eq!(n.edges[0], Some('L'));
        assert_eq!(n.value(0, 0, 0), Some(20.0));
    }

    #[test]
    fn strided_and_decimated_reads_agree_with_the_plain_ones() {
        let v = ramp();
        let m = v.default_map().unwrap();
        let s = v.extract(&m, &view(PlaneKind::Sagittal, 0), ChannelSel::All, Some(1)).unwrap();
        assert_eq!((s.width, s.height), (1, 1));
        // The mean of the first column across y and z: x = 0.
        let mean = [0.0, 10.0, 20.0, 100.0, 110.0, 120.0].iter().sum::<f32>() / 6.0;
        assert!((s.data[0] - mean).abs() < 1e-3, "{} vs {mean}", s.data[0]);
    }

    #[test]
    fn colour_samples_stay_together() {
        // 2 x 2 RGB bytes.
        let bytes: Vec<u8> = (0..12).collect();
        let axes = vec![Axis::new(2, 6, Role::Y), Axis::new(2, 3, Role::X), Axis::new(3, 1, Role::Color)];
        let v = Volume::new("rgb", "test", axes, DType::U8, Endian::Little, Arc::new(MemSource(bytes)));
        let m = v.default_map().unwrap();
        let s = v.extract(&m, &view(PlaneKind::Axial, 0), ChannelSel::All, None).unwrap();
        assert_eq!(s.samples, 3);
        assert_eq!(&s.data[3..6], &[3.0, 4.0, 5.0]);
        let g = v.extract(&m, &view(PlaneKind::Axial, 0), ChannelSel::One(1), None).unwrap();
        assert_eq!(g.data, vec![1.0, 4.0, 7.0, 10.0]);
    }

    #[test]
    fn short_data_clips_the_slowest_axis() {
        let mut v = ramp();
        let bytes = v.source.as_slice().unwrap()[..(12 + 5) * 4].to_vec();
        v.source = Arc::new(MemSource(bytes));
        v.fit().unwrap();
        assert_eq!(v.axes[0].len, 1);
        assert_eq!(v.warnings.len(), 1);
        let mut w = ramp();
        w.source = Arc::new(MemSource(vec![0; 8]));
        assert!(w.fit().is_err());
    }

    #[test]
    fn absurd_sizes_are_refused_without_overflow() {
        let axes = vec![Axis::new(usize::MAX / 2, usize::MAX / 4, Role::Y), Axis::new(usize::MAX / 2, 1, Role::X)];
        let mut v = Volume::new("x", "t", axes, DType::F64, Endian::Little, Arc::new(MemSource(vec![0; 16])));
        assert!(v.fit().is_err());
        let axes = vec![Axis::new(0, 1, Role::X)];
        let mut v = Volume::new("x", "t", axes, DType::F64, Endian::Little, Arc::new(MemSource(vec![0; 16])));
        assert!(v.fit().is_err());
    }

    #[test]
    fn statistics_cover_the_range() {
        let v = ramp();
        let st = v.stats().unwrap();
        assert_eq!(st.min, 0.0);
        assert_eq!(st.max, 123.0);
        assert_eq!(st.sampled, 24);
        assert_eq!(st.histogram.iter().sum::<u32>(), 24);
    }

    #[test]
    fn maximum_projection_takes_the_brightest() {
        let v = ramp();
        let m = v.default_map().unwrap();
        let p = v.mip(&m, &view(PlaneKind::Axial, 0), ChannelSel::All).unwrap();
        assert_eq!(p.value(0, 0, 0), Some(100.0));
        assert_eq!(p.value(3, 2, 0), Some(123.0));
    }

    #[test]
    fn slice_axis_choices_rotate_the_roles() {
        let mut v = ramp();
        v.slice_axis_guess = true;
        let c = v.slice_axis_choices();
        assert_eq!(c.len(), 3);
        assert_eq!(c[1].1.z, Some(1));
        assert_eq!((c[1].1.y, c[1].1.x), (0, 2));
        // Choosing axis 2 as the slice axis shows axes 0 and 1 as rows and columns.
        let s = v.extract(&c[2].1, &view(PlaneKind::Axial, 3), ChannelSel::All, None).unwrap();
        assert_eq!((s.width, s.height), (3, 2));
    }
}
