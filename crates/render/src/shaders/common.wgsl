// Shared by every scene shader: the per-pass frame uniforms, the per-draw
// records, vertex pulling from the geometry buffer, skinning, morph targets,
// colour encoding and tone mapping.
//
// Vertex data is not bound as vertex buffers.  Each geometry chunk is one
// storage buffer of 32-bit words holding indices, attributes and morph
// deltas, and each draw record says where its attributes start.  That keeps
// one pipeline for every combination of attributes, which matters for cold
// starts, where each pipeline costs compile time.

const NONE: u32 = 0xffffffffu;

struct Frame {
    view_proj: mat4x4f,
    view: mat4x4f,
    inv_view_proj: mat4x4f,
    light_view_proj: mat4x4f,
    contact_view_proj: mat4x4f,
    // xyz position; w = 1 for an orthographic camera.
    camera: vec4f,
    // xyz the direction the camera looks.
    camera_forward: vec4f,
    // xyz towards the key light; w = shadow strength, 0 without a key light.
    light_dir: vec4f,
    // rgb radiance of the key light; w = one shadow map texel in UV units.
    light_color: vec4f,
    // cos and sin of the environment's rotation, its intensity, and the
    // radiance cube's largest mip level.
    env: vec4f,
    // exposure multiplier, tone map (0 none, 1 PBR Neutral, 2 ACES),
    // 1 when the target is sRGB (the hardware encodes), background blur lod.
    tone: vec4f,
    // ground height, contact shadow height, ground radius, contact strength.
    ground: vec4f,
    // ground centre x and z, grid spacing, grid radius.
    ground_center: vec4f,
    // width, height, 1/width, 1/height.
    viewport: vec4f,
    // time in seconds, overlay line length, debug channel, wireframe alpha.
    misc: vec4f,
    // Background colour, linear and premultiplied.
    background: vec4f,
    // Shadow normal offset (world units), depth bias, contact blur radius
    // (UV units), 1 when the scene itself shows the key light's shadow.
    shadow: vec4f,
    // Ambient occlusion's strength (0 when off), the fill light (the
    // fraction of a lit surface's own colour shown where no light falls),
    // 0, 0.
    ao: vec4f,
    // Points: size multiplier, 1 when dots are sized in the world, pixels
    // per world unit when the scene fills the view, multisamples.
    points: vec4f,
    // The projection's x and y scales (P[0][0] and P[1][1]), 0, 0.
    proj: vec4f,
    // log2 of the transmission texture's larger side, 1 when it holds this
    // frame's opaque scene, 0, 0.
    transmission: vec4f,
}

struct Draw {
    model: mat4x4f,
    // Inverse transpose of the model matrix, for normals.
    normal: mat4x4f,
    // Word offsets of positions, normals, tangents, uv0 (NONE = absent).
    attrs0: vec4u,
    // Word offsets of uv1, colours, joints, weights.
    attrs1: vec4u,
    // First active morph, active morph count, vertex count, 1 when the
    // morph targets carry normal deltas.
    morph: vec4u,
    // Joint matrix base, index word offset, the point spacing in world
    // units as f32 bits (points only), material index.
    misc: vec4u,
}

@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var<storage, read> draws: array<Draw>;
@group(0) @binding(2) var<storage, read> joints: array<mat4x4f>;
// Active morph targets: word offset of the target's position deltas and the
// weight's bits.
@group(0) @binding(3) var<storage, read> morphs: array<vec2u>;
// Draw indices: instance i of a call draws record draws[instances[i]], so
// one call draws every visible copy of a primitive (see instancing.rs).
@group(0) @binding(4) var<storage, read> instances: array<u32>;

// The draw record of an instance.
fn instance_draw(i: u32) -> Draw {
    return draws[instances[i]];
}

@group(2) @binding(0) var<storage, read> geo: array<u32>;

fn load_f(i: u32) -> f32 {
    return bitcast<f32>(geo[i]);
}

fn load2(base: u32, v: u32) -> vec2f {
    let i = base + v * 2u;
    return vec2f(load_f(i), load_f(i + 1u));
}

fn load3(base: u32, v: u32) -> vec3f {
    let i = base + v * 3u;
    return vec3f(load_f(i), load_f(i + 1u), load_f(i + 2u));
}

fn load4(base: u32, v: u32) -> vec4f {
    let i = base + v * 4u;
    return vec4f(load_f(i), load_f(i + 1u), load_f(i + 2u), load_f(i + 3u));
}

struct Vertex {
    pos: vec3f,
    normal: vec3f,
    tangent: vec4f,
    uv0: vec2f,
    uv1: vec2f,
    color: vec4f,
}

// Reads vertex `v` of draw `d` in the draw's model space (world space for
// skinned meshes), with morph targets and skinning applied.
fn fetch_vertex(d: Draw, v: u32) -> Vertex {
    var o: Vertex;
    o.pos = load3(d.attrs0.x, v);
    o.normal = vec3f(0.0, 0.0, 0.0);
    if d.attrs0.y != NONE {
        o.normal = load3(d.attrs0.y, v);
    }
    o.tangent = vec4f(0.0);
    if d.attrs0.z != NONE {
        o.tangent = load4(d.attrs0.z, v);
    }
    o.uv0 = vec2f(0.0);
    if d.attrs0.w != NONE {
        o.uv0 = load2(d.attrs0.w, v);
    }
    o.uv1 = o.uv0;
    if d.attrs1.x != NONE {
        o.uv1 = load2(d.attrs1.x, v);
    }
    o.color = vec4f(1.0);
    if d.attrs1.y != NONE {
        o.color = load4(d.attrs1.y, v);
    }

    // Morph targets: only the active ones, so meshes with hundreds of
    // targets cost no more than the few that are weighted.
    let normal_skip = d.morph.z * 3u;
    for (var i = 0u; i < d.morph.y; i++) {
        let m = morphs[d.morph.x + i];
        let w = bitcast<f32>(m.y);
        o.pos += load3(m.x, v) * w;
        if d.morph.w != 0u {
            o.normal += load3(m.x + normal_skip, v) * w;
        }
    }

    // Skinning: joint indices are four u16 in two words.
    if d.attrs1.z != NONE && d.attrs1.w != NONE {
        let j0 = geo[d.attrs1.z + v * 2u];
        let j1 = geo[d.attrs1.z + v * 2u + 1u];
        let base = d.misc.x;
        let w = load4(d.attrs1.w, v);
        let m = joints[base + (j0 & 0xffffu)] * w.x + joints[base + (j0 >> 16u)] * w.y
            + joints[base + (j1 & 0xffffu)] * w.z + joints[base + (j1 >> 16u)] * w.w;
        o.pos = (m * vec4f(o.pos, 1.0)).xyz;
        o.normal = (m * vec4f(o.normal, 0.0)).xyz;
        o.tangent = vec4f((m * vec4f(o.tangent.xyz, 0.0)).xyz, o.tangent.w);
    }
    return o;
}

// The vertex index a non-indexed draw stands for, for passes that draw
// every triangle corner separately (the wireframe overlay).
fn indexed_vertex(d: Draw, corner: u32) -> u32 {
    return geo[d.misc.y + corner];
}




// Exposure and tone mapping: scene-referred linear light in, display-
// referred linear light out.
fn tonemap(c: vec3f) -> vec3f {
    return tonemap_mode(c * frame.tone.x, u32(frame.tone.y + 0.5));
}

// Display-referred linear light to what the target stores: sRGB targets
// encode in hardware, others need it done here.
fn to_target(c: vec3f) -> vec3f {
    if frame.tone.z > 0.5 {
        return c;
    }
    return linear_to_srgb(c);
}

// For debug views that show raw values: makes the target display `c`
// exactly, undoing the encoding an sRGB target applies.
fn raw_to_target(c: vec3f) -> vec3f {
    if frame.tone.z > 0.5 {
        return srgb_to_linear(clamp(c, vec3f(0.0), vec3f(1.0)));
    }
    return c;
}

// Rotates a world direction into the environment's frame.
fn env_dir(d: vec3f) -> vec3f {
    let c = frame.env.x;
    let s = frame.env.y;
    return vec3f(c * d.x - s * d.z, d.y, s * d.x + c * d.z);
}

