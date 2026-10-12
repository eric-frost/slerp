// Gaussian splats, step 1 of each frame: project every splat to the screen.
//
// For each splat this computes the screen-space ellipse of its 3D
// covariance (the EWA approximation of 3D Gaussian splatting: the
// projection's Jacobian at the centre, plus a 0.3 pixel low-pass filter),
// its colour for the direction it is seen from (spherical harmonics up to
// degree 3), and, for the GPU sort, a depth key.  The draw then reads the
// result in sorted order, so the ellipse and colour are worked out once per
// splat rather than once per corner.

struct Params {
    view: mat4x4f,
    proj: mat4x4f,
    // xyz camera position; w = 1 for an orthographic camera.
    camera: vec4f,
    // xyz the direction the camera looks.
    forward: vec4f,
    // Viewport width and height, focal lengths in pixels (x, y).
    viewport: vec4f,
    // Splat size multiplier, near plane depth, and the tangents of the
    // half fields of view (x, y), for clamping the Jacobian.
    misc: vec4f,
    // Splat count, spherical-harmonic degree, harmonic words per splat,
    // flags: 1 = antialiased (Mip-Splatting), 2 = write sort keys.
    counts: vec4u,
}

// What the draw needs per splat: the centre in normalised device
// coordinates, the ellipse's two half-axes in pixels (as pairs of half
// floats) reaching three standard deviations, and the colour and opacity
// (8 bits each, display-encoded).  Opacity 0 means culled.
struct Projected {
    center: vec2f,
    axis1: u32,
    axis2: u32,
    color: u32,
    pad: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
// Per splat, 12 words: centre xyz, opacity, covariance (xx, xy, xz, yy, yz,
// zz) as float bits, and the base colour as half floats (rg, b0).
@group(0) @binding(1) var<storage, read> base: array<u32>;
// Per splat: `counts.z` words of half-float harmonic coefficients,
// coefficient-major, red, green, blue.
@group(0) @binding(2) var<storage, read> harmonics: array<u32>;
@group(0) @binding(3) var<storage, read_write> projected: array<Projected>;
@group(0) @binding(4) var<storage, read_write> keys: array<u32>;
@group(0) @binding(5) var<storage, read_write> values: array<u32>;

const SH_C1: f32 = 0.4886025119029199;
const SH_C2_0: f32 = 1.0925484305920792;
const SH_C2_1: f32 = -1.0925484305920792;
const SH_C2_2: f32 = 0.31539156525252005;
const SH_C2_3: f32 = -1.0925484305920792;
const SH_C2_4: f32 = 0.5462742152960396;
const SH_C3_0: f32 = -0.5900435899266435;
const SH_C3_1: f32 = 2.890611442640554;
const SH_C3_2: f32 = -0.4570457994644658;
const SH_C3_3: f32 = 0.3731763325901154;
const SH_C3_4: f32 = -0.4570457994644658;
const SH_C3_5: f32 = 1.445305721320277;
const SH_C3_6: f32 = -0.5900435899266435;

fn f(i: u32) -> f32 {
    return bitcast<f32>(base[i]);
}

fn half_at(first: u32, h: u32) -> f32 {
    let pair = unpack2x16float(harmonics[first + (h >> 1u)]);
    return select(pair.x, pair.y, (h & 1u) == 1u);
}

// Coefficient `k` (0 to 14) of the splat whose words start at `first`.
fn coef(first: u32, k: u32) -> vec3f {
    let h = k * 3u;
    return vec3f(half_at(first, h), half_at(first, h + 1u), half_at(first, h + 2u));
}

// The colour bands 1 to 3 add for viewing direction `d` (unit length, from
// the camera to the splat), as the 3D Gaussian splatting reference code
// evaluates them.
fn harmonic_color(i: u32, d: vec3f) -> vec3f {
    let degree = p.counts.y;
    if degree == 0u {
        return vec3f(0.0);
    }
    let first = i * p.counts.z;
    let x = d.x;
    let y = d.y;
    let z = d.z;
    var c = SH_C1 * (-y * coef(first, 0u) + z * coef(first, 1u) - x * coef(first, 2u));
    if degree > 1u {
        let xx = x * x;
        let yy = y * y;
        let zz = z * z;
        c += SH_C2_0 * x * y * coef(first, 3u) + SH_C2_1 * y * z * coef(first, 4u)
            + SH_C2_2 * (2.0 * zz - xx - yy) * coef(first, 5u) + SH_C2_3 * x * z * coef(first, 6u)
            + SH_C2_4 * (xx - yy) * coef(first, 7u);
        if degree > 2u {
            c += SH_C3_0 * y * (3.0 * xx - yy) * coef(first, 8u) + SH_C3_1 * x * y * z * coef(first, 9u)
                + SH_C3_2 * y * (4.0 * zz - xx - yy) * coef(first, 10u)
                + SH_C3_3 * z * (2.0 * zz - 3.0 * xx - 3.0 * yy) * coef(first, 11u)
                + SH_C3_4 * x * (4.0 * zz - xx - yy) * coef(first, 12u) + SH_C3_5 * z * (xx - yy) * coef(first, 13u)
                + SH_C3_6 * x * (xx - 3.0 * yy) * coef(first, 14u);
        }
    }
    return c;
}

// A 24-bit key that sorts splats farthest first: the depth's bits made to
// order like the number (negative depths too, for orthographic views),
// inverted, keeping the top 24 bits (15 bits of mantissa, about 0.003%).
fn depth_key(depth: f32) -> u32 {
    let b = bitcast<u32>(depth);
    let ordered = select(b | 0x80000000u, ~b, (b & 0x80000000u) != 0u);
    return (~ordered) >> 8u;
}

@compute @workgroup_size(256)
fn cs_project(@builtin(global_invocation_id) gid: vec3u, @builtin(num_workgroups) groups: vec3u) {
    let i = gid.x + gid.y * groups.x * 256u;
    if i >= p.counts.x {
        return;
    }
    let b = i * 12u;
    let center = vec3f(f(b), f(b + 1u), f(b + 2u));
    let view_pos = (p.view * vec4f(center, 1.0)).xyz;
    let depth = -view_pos.z;
    if (p.counts.w & 2u) != 0u {
        keys[i] = depth_key(depth);
        values[i] = i;
    }
    var out: Projected;
    out.color = 0u;
    let ortho = p.camera.w > 0.5;

    // 1. Cull splats behind the camera and centres well off screen.
    let clip = p.proj * vec4f(view_pos, 1.0);
    if (!ortho && depth < p.misc.y) || clip.w <= 0.0 {
        projected[i] = out;
        return;
    }
    let ndc = clip.xy / clip.w;
    if abs(ndc.x) > 1.3 || abs(ndc.y) > 1.3 {
        projected[i] = out;
        return;
    }

    // 2. The screen-space covariance: J W V Wᵀ Jᵀ, with V the splat's 3D
    //    covariance, W the view's rotation and J the projection's Jacobian
    //    in pixels.  Off-screen centres are clamped as the reference code
    //    does, so the Jacobian doesn't explode at the edges.
    let s2 = p.misc.x * p.misc.x;
    let v = mat3x3f(
        vec3f(f(b + 4u), f(b + 5u), f(b + 6u)),
        vec3f(f(b + 5u), f(b + 7u), f(b + 8u)),
        vec3f(f(b + 6u), f(b + 8u), f(b + 9u))
    ) * s2;
    let w = mat3x3f(p.view[0].xyz, p.view[1].xyz, p.view[2].xyz);
    let fx = p.viewport.z;
    let fy = p.viewport.w;
    var j: mat3x3f;
    if ortho {
        j = mat3x3f(vec3f(fx, 0.0, 0.0), vec3f(0.0, fy, 0.0), vec3f(0.0));
    } else {
        let tx = clamp(view_pos.x / depth, -1.3 * p.misc.z, 1.3 * p.misc.z) * depth;
        let ty = clamp(view_pos.y / depth, -1.3 * p.misc.w, 1.3 * p.misc.w) * depth;
        let d2 = depth * depth;
        j = mat3x3f(vec3f(fx / depth, 0.0, 0.0), vec3f(0.0, fy / depth, 0.0), vec3f(fx * tx / d2, fy * ty / d2, 0.0));
    }
    let t = j * w;
    let cov = t * v * transpose(t);
    var a = cov[0][0];
    let bb = cov[0][1];
    var c = cov[1][1];
    let det_before = a * c - bb * bb;
    a += 0.3;
    c += 0.3;
    let det = a * c - bb * bb;
    if !(det > 0.0) {
        projected[i] = out;
        return;
    }
    // Mip-Splatting trains with the filter's widening compensated in
    // opacity, so it is compensated here too.
    var opacity = f(b + 3u);
    if (p.counts.w & 1u) != 0u {
        opacity *= sqrt(max(det_before, 0.0) / det);
    }

    // 3. The ellipse's axes from the eigenvalues, out to 3 standard
    //    deviations, at most 4096 pixels.
    let mid = 0.5 * (a + c);
    let r = length(vec2f(0.5 * (a - c), bb));
    let l1 = mid + r;
    let l2 = max(mid - r, 0.1);
    var dir = vec2f(1.0, 0.0);
    if abs(bb) > 1e-9 || c > a {
        dir = normalize(vec2f(bb, l1 - a));
    }
    let r1 = min(3.0 * sqrt(l1), 4096.0);
    let r2 = min(3.0 * sqrt(l2), 4096.0);

    // 4. Colour for the direction from the camera.
    var view_dir = p.forward.xyz;
    if !ortho {
        view_dir = normalize(center - p.camera.xyz);
    }
    let rg = unpack2x16float(base[b + 10u]);
    let b0 = unpack2x16float(base[b + 11u]);
    let color = max(vec3f(rg, b0.x) + harmonic_color(i, view_dir), vec3f(0.0));
    if opacity < 1.0 / 255.0 {
        projected[i] = out;
        return;
    }
    out.center = ndc;
    out.axis1 = pack2x16float(dir * r1);
    out.axis2 = pack2x16float(vec2f(-dir.y, dir.x) * r2);
    out.color = pack4x8unorm(vec4f(min(color, vec3f(1.0)), opacity));
    projected[i] = out;
}
