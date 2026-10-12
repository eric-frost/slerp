// Screen-space ambient occlusion: ground-truth ambient occlusion (GTAO,
// Jimenez et al. 2016) as Intel's XeGTAO (MIT) computes it, then a
// depth-aware blur.  Prefixed with color.wgsl.
//
// For each pixel, a few slices through the view direction are searched
// for the highest horizon on either side within a radius, and the
// cosine-weighted visibility of the sky between the two horizons is
// integrated analytically around the pixel's normal.  A short march along
// the mirror direction adds what the horizons can't see: whether the
// reflection runs into nearby geometry hidden behind the surface, such as
// the neck under a jaw seen from the front, whose sheen would otherwise
// stay.  Each pixel of a
// 4x4 block turns its slices and offsets its steps by a different
// amount, and the blur's tent filter weights every one of the 16 cases
// alike, so one frame gives a smooth result without temporal
// accumulation, which thumbnails don't have.

struct AoParams {
    proj: mat4x4f,
    inv_proj: mat4x4f,
    // Towards the key light in view space (towards the camera when the key
    // light is off), for shading plain point clouds.
    light: vec4f,
    // Effect radius in view units, pixels per view unit at view depth 1
    // (or anywhere, for an orthographic camera), the largest screen radius
    // in pixels, 1 for an orthographic camera.
    params: vec4f,
    // width, height, 1/width, 1/height.
    viewport: vec4f,
}

@group(0) @binding(0) var<uniform> ao: AoParams;
@group(0) @binding(1) var depth_map: texture_depth_2d;
// View-space normals (zero where the pre-pass drew nothing); w is the
// distance within which neighbours don't occlude (point clouds).
@group(0) @binding(2) var normal_map: texture_2d<f32>;
// The noisy visibility and reflection visibility, for the blur.
@group(0) @binding(3) var raw_ao: texture_2d<f32>;

const SLICES: u32 = 3u;
const STEPS: u32 = 6u;
const MIRROR_STEPS: u32 = 8u;
// XeGTAO's defaults: the outer 61.5% of the radius fades out, steps
// crowd towards the centre, and a sample closer than 1.3 pixels is no
// sample.
const FALLOFF: f32 = 0.615;
const TOO_CLOSE: f32 = 1.3;
const BAYER = array<u32, 16>(0u, 8u, 2u, 10u, 12u, 4u, 14u, 6u, 3u, 11u, 1u, 9u, 15u, 7u, 13u, 5u);

struct ScreenOut {
    @builtin(position) clip: vec4f,
}

@vertex
fn vs_ao(@builtin(vertex_index) i: u32) -> ScreenOut {
    var o: ScreenOut;
    o.clip = fullscreen(i);
    return o;
}

fn clamp_px(p: vec2i) -> vec2i {
    return clamp(p, vec2i(0), vec2i(ao.viewport.xy) - 1);
}

// The view-space position of pixel `p` at depth-buffer value `d`.
fn view_pos(p: vec2i, d: f32) -> vec3f {
    let uv = (vec2f(p) + 0.5) * ao.viewport.zw;
    let h = ao.inv_proj * vec4f(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, d, 1.0);
    return h.xyz / h.w;
}

fn fast_acos(x: f32) -> f32 {
    return acos(clamp(x, -1.0, 1.0));
}

@fragment
fn fs_gtao(in: ScreenOut) -> @location(0) vec4f {
    let px = vec2i(in.clip.xy);
    let d = textureLoad(depth_map, px, 0);
    let nw = textureLoad(normal_map, px, 0);
    if d >= 1.0 || dot(nw.xyz, nw.xyz) < 0.25 {
        return vec4f(1.0);
    }
    let p = view_pos(px, d);
    let near = nw.w;
    let n = normalize(nw.xyz);
    let ortho = ao.params.w > 0.5;
    let v = select(normalize(-p), vec3f(0.0, 0.0, 1.0), ortho);
    let radius = ao.params.x;
    let px_per_unit = select(ao.params.y / max(-p.z, 1e-6), ao.params.y, ortho);
    let screen_radius = min(radius * px_per_unit, ao.params.z);
    if screen_radius < TOO_CLOSE {
        return vec4f(1.0);
    }
    let falloff_range = FALLOFF * radius;
    let falloff_mul = -1.0 / falloff_range;
    let falloff_add = (radius - falloff_range) / falloff_range + 1.0;
    let min_s = TOO_CLOSE / screen_radius;

    // Every pixel of a 4x4 block turns its slices and offsets its steps
    // differently (7 is coprime with 16, so the steps get every offset).
    let k = BAYER[u32(px.y & 3) * 4u + u32(px.x & 3)];
    let slice_noise = (f32(k) + 0.5) / 16.0;
    let step_noise = (f32((k * 7u + 3u) & 15u) + 0.5) / 16.0;

    var visibility = 0.0;
    for (var s = 0u; s < SLICES; s++) {
        let phi = (f32(s) + slice_noise) * PI / f32(SLICES);
        let dir = vec3f(cos(phi), sin(phi), 0.0);
        // Screen y points down, view y up.
        let omega = vec2f(dir.x, -dir.y) * screen_radius;
        let ortho_dir = dir - v * dot(dir, v);
        let axis = normalize(cross(ortho_dir, v));
        let proj_n = n - axis * dot(n, axis);
        let proj_len = length(proj_n);
        let sign_n = select(-1.0, 1.0, dot(ortho_dir, proj_n) >= 0.0);
        let cos_n = clamp(dot(proj_n, v) / max(proj_len, 1e-6), 0.0, 1.0);
        let angle_n = sign_n * fast_acos(cos_n);
        let low0 = cos(angle_n + PI * 0.5);
        let low1 = cos(angle_n - PI * 0.5);
        var h_cos0 = low0;
        var h_cos1 = low1;
        for (var j = 0u; j < STEPS; j++) {
            var t = (f32(j) + step_noise) / f32(STEPS);
            t = t * t + min_s;
            let offset = vec2i(round(omega * t));
            let q0 = clamp_px(px + offset);
            let q1 = clamp_px(px - offset);
            let d0 = textureLoad(depth_map, q0, 0);
            let d1 = textureLoad(depth_map, q1, 0);
            let delta0 = view_pos(q0, d0) - p;
            let delta1 = view_pos(q1, d1) - p;
            let dist0 = length(delta0);
            let dist1 = length(delta1);
            var w0 = clamp(dist0 * falloff_mul + falloff_add, 0.0, 1.0);
            var w1 = clamp(dist1 * falloff_mul + falloff_add, 0.0, 1.0);
            if near > 0.0 {
                w0 *= clamp(dist0 / near - 1.0, 0.0, 1.0);
                w1 *= clamp(dist1 / near - 1.0, 0.0, 1.0);
            }
            // The far plane (nothing drawn) is no occluder.
            let shc0 = select(low0, mix(low0, dot(delta0, v) / max(dist0, 1e-9), w0), d0 < 1.0);
            let shc1 = select(low1, mix(low1, dot(delta1, v) / max(dist1, 1e-9), w1), d1 < 1.0);
            h_cos0 = max(h_cos0, shc0);
            h_cos1 = max(h_cos1, shc1);
        }
        // XeGTAO's correction for the horizon falloff.
        let len = mix(proj_len, 1.0, 0.05);
        var h0 = -fast_acos(h_cos1);
        var h1 = fast_acos(h_cos0);
        h0 = angle_n + clamp(h0 - angle_n, -PI * 0.5, PI * 0.5);
        h1 = angle_n + clamp(h1 - angle_n, -PI * 0.5, PI * 0.5);
        let sin_n = sin(angle_n);
        let arc0 = (cos_n + 2.0 * h0 * sin_n - cos(2.0 * h0 - angle_n)) * 0.25;
        let arc1 = (cos_n + 2.0 * h1 * sin_n - cos(2.0 * h1 - angle_n)) * 0.25;
        visibility += len * (arc0 + arc1);
    }
    visibility = clamp(visibility / f32(SLICES), 0.03, 1.0);
    return vec4f(visibility, mirror_visibility(p, n, v, radius, near, step_noise), 0.0, 0.0);
}

// How much of the reflection along the mirror direction escapes: a march
// out to the radius, where the first step that lands behind the depth
// buffer's surface, but by less than the radius (thinner things in front
// don't count), ends it.  Nearer hits hide more.  A step lands on a pixel
// centre, where a surface sloping away from the camera is deeper than
// where the step really is, so it must be behind by more than that slope
// across a pixel (capped, or surfaces seen edge-on would never occlude).
fn mirror_visibility(p: vec3f, n: vec3f, v: vec3f, radius: f32, near: f32, noise: f32) -> f32 {
    let ortho = ao.params.w > 0.5;
    let r = reflect(-v, n);
    for (var i = 0u; i < MIRROR_STEPS; i++) {
        let t = (f32(i) + noise) / f32(MIRROR_STEPS) * radius;
        if t <= near {
            continue;
        }
        let q = p + r * t;
        let c = ao.proj * vec4f(q, 1.0);
        if c.w <= 0.0 {
            break;
        }
        let ndc = c.xy / c.w;
        if any(abs(ndc) > vec2f(1.0)) {
            break;
        }
        let px = clamp_px(vec2i((vec2f(ndc.x, -ndc.y) * 0.5 + 0.5) * ao.viewport.xy));
        let d = textureLoad(depth_map, px, 0);
        if d >= 1.0 {
            continue;
        }
        let b = view_pos(px, d);
        let nb = textureLoad(normal_map, px, 0).xyz;
        let pixel = select(-b.z, 1.0, ortho) / ao.params.y;
        let slope = pixel * length(nb.xy) / max(abs(nb.z), 1e-3);
        let behind = b.z - q.z;
        if behind > max(radius * 0.02, min(slope, radius * 0.25)) && behind < radius {
            return t / radius;
        }
    }
    return 1.0;
}

// Blurs both visibilities over 5x5 pixels of the same surface.  The tent
// weights (1, 2, 2, 2, 1) along each axis give each of the four noise
// rows and columns the same total weight.  Writes the visibility, the view
// depth it belongs to, which surfaces check against their own depth, the
// reflection's visibility, and for plain point clouds a shade from the
// surface the dots' depths make (`cloud_shade`).
@fragment
fn fs_ao_blur(in: ScreenOut) -> @location(0) vec4f {
    let px = vec2i(in.clip.xy);
    let d = textureLoad(depth_map, px, 0);
    let nw = textureLoad(normal_map, px, 0);
    if d >= 1.0 || dot(nw.xyz, nw.xyz) < 0.25 {
        return vec4f(1.0, 0.0, 1.0, 0.0);
    }
    let p = view_pos(px, d);
    let n = normalize(nw.xyz);
    let depth = -p.z;
    let ortho = ao.params.w > 0.5;
    let px_per_unit = select(ao.params.y / max(depth, 1e-6), ao.params.y, ortho);
    // Samples off the pixel's tangent plane by more than about a pixel
    // and a half belong to another surface.
    let tolerance = max(1.5 / px_per_unit, depth * 0.002);
    var sum = vec2f(0.0);
    var total = 0.0;
    for (var y = -2; y <= 2; y++) {
        for (var x = -2; x <= 2; x++) {
            let tent = select(2.0, 1.0, abs(x) == 2) * select(2.0, 1.0, abs(y) == 2);
            let q = clamp_px(px + vec2i(x, y));
            let dq = textureLoad(depth_map, q, 0);
            if dq >= 1.0 {
                continue;
            }
            let off_plane = abs(dot(view_pos(q, dq) - p, n));
            let w = tent * clamp(1.0 - off_plane / tolerance, 0.0, 1.0);
            sum += textureLoad(raw_ao, q, 0).rg * w;
            total += w;
        }
    }
    let both = select(vec2f(1.0), sum / total, total > 0.0);
    var shade = 1.0;
    if nw.w > 0.0 && nw.w < 60000.0 {
        shade = cloud_shade(px, p, nw.w);
    }
    return vec4f(both.x, depth, both.y, shade);
}

// Points carry no normals, so a plain cloud would show as a flat
// silhouette.  The surface its dots form in the depth buffer has one:
// differences of positions a dot's width apart, on whichever side stays on
// the same surface, give it, and the key light shades it.
fn cloud_shade(px: vec2i, p: vec3f, dot_width: f32) -> f32 {
    let ortho = ao.params.w > 0.5;
    let px_per_unit = select(ao.params.y / max(-p.z, 1e-6), ao.params.y, ortho);
    let step = i32(clamp(dot_width * px_per_unit, 2.0, 8.0));
    var sides: array<vec3f, 4>;
    let offsets = array<vec2i, 4>(vec2i(step, 0), vec2i(-step, 0), vec2i(0, step), vec2i(0, -step));
    for (var i = 0; i < 4; i++) {
        let q = clamp_px(px + offsets[i]);
        let d = textureLoad(depth_map, q, 0);
        sides[i] = select(view_pos(q, d) - p, vec3f(1e9), d >= 1.0);
    }
    // The nearer neighbour along each axis, turned to point the same way.
    let dx = select(-sides[1], sides[0], abs(sides[0].z) < abs(sides[1].z));
    let dy = select(-sides[3], sides[2], abs(sides[2].z) < abs(sides[3].z));
    var n = cross(dx, dy);
    if dot(n, n) < 1e-20 || any(abs(dx) > vec3f(1e8)) || any(abs(dy) > vec3f(1e8)) {
        return 1.0;
    }
    n = normalize(n);
    // Screen y points down, so the cross product faces away; face the
    // camera.
    if n.z < 0.0 {
        n = -n;
    }
    return 0.45 + 0.65 * max(dot(n, ao.light.xyz), 0.0);
}
