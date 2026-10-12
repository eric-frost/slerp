// Gaussian splats, step 3: draw each projected splat, in sorted order, as a
// quad covering its ellipse out to three standard deviations, with the
// Gaussian's falloff as alpha.  The splats blend back to front, with
// premultiplied alpha, into a layer of their own that holds display-encoded
// colour: 3D Gaussian splatting trains on blending there, in the photos'
// encoding, and blending in linear light instead makes soft edges and
// translucent parts look different from what was trained.

struct Params {
    view: mat4x4f,
    proj: mat4x4f,
    camera: vec4f,
    forward: vec4f,
    // Viewport width and height, focal lengths in pixels.
    viewport: vec4f,
    misc: vec4f,
    counts: vec4u,
}

struct Projected {
    center: vec2f,
    axis1: u32,
    axis2: u32,
    color: u32,
    pad: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> projected: array<Projected>;
// Splat indices, farthest first.
@group(0) @binding(2) var<storage, read> order: array<u32>;

struct SplatOut {
    @builtin(position) clip: vec4f,
    // Position within the quad, -1 to 1 on each axis: 3 standard deviations.
    @location(0) local: vec2f,
    @location(1) color: vec4f,
}

@vertex
fn vs_splat(@builtin(vertex_index) v: u32, @builtin(instance_index) n: u32) -> SplatOut {
    var o: SplatOut;
    let s = projected[order[n]];
    if (s.color >> 24u) == 0u {
        // Culled: outside the clip volume, so nothing rasterises.
        o.clip = vec4f(0.0, 0.0, 2.0, 1.0);
        return o;
    }
    let corner = vec2f(f32(v & 1u), f32(v >> 1u)) * 2.0 - 1.0;
    let offset = corner.x * unpack2x16float(s.axis1) + corner.y * unpack2x16float(s.axis2);
    o.clip = vec4f(s.center + offset * 2.0 / p.viewport.xy, 0.0, 1.0);
    o.local = corner;
    o.color = unpack4x8unorm(s.color);
    return o;
}

@fragment
fn fs_splat(in: SplatOut) -> @location(0) vec4f {
    let r2 = dot(in.local, in.local);
    if r2 > 1.0 {
        discard;
    }
    // exp(-x²/2) with x in standard deviations, 3 at the quad's edge.
    let alpha = min(0.99, in.color.a * exp(-4.5 * r2));
    if alpha < 1.0 / 255.0 {
        discard;
    }
    return vec4f(in.color.rgb * alpha, alpha);
}
