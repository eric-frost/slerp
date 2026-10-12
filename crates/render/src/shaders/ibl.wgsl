// Image-based lighting, generated on the GPU so nothing large ships with
// the program: the procedural studio environment, conversion of an
// equirectangular HDR or a cubemap, mip reduction, GGX prefiltering of the
// radiance cube, irradiance as spherical harmonics, and the split-sum BRDF
// table.  Prefixed with color.wgsl.
//
// Each entry point uses its own subset of these bindings; pipelines derive
// their layouts from what the entry point touches.

struct Params {
    // Destination size, sample count, 0, 0.
    a: vec4u,
    // Roughness, level of detail, source face size, source mip count.
    b: vec4f,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var dst: texture_storage_2d_array<rgba16float, write>;
@group(0) @binding(2) var src_equirect: texture_2d<f32>;
@group(0) @binding(3) var src_cube: texture_cube<f32>;
@group(0) @binding(4) var src_sampler: sampler;
@group(0) @binding(5) var src_level: texture_2d_array<f32>;
@group(0) @binding(6) var<storage, read_write> sh_out: array<vec4f, 9>;

// The direction through texel centre `uv` (-1..1, v downwards) of a cube
// face, in Vulkan's face order +X, -X, +Y, -Y, +Z, -Z.
fn cube_dir(face: u32, uv: vec2f) -> vec3f {
    switch face {
        case 0u: { return normalize(vec3f(1.0, -uv.y, -uv.x)); }
        case 1u: { return normalize(vec3f(-1.0, -uv.y, uv.x)); }
        case 2u: { return normalize(vec3f(uv.x, 1.0, uv.y)); }
        case 3u: { return normalize(vec3f(uv.x, -1.0, -uv.y)); }
        case 4u: { return normalize(vec3f(uv.x, -uv.y, 1.0)); }
        default: { return normalize(vec3f(-uv.x, -uv.y, -1.0)); }
    }
}

fn texel_dir(id: vec3u, size: u32) -> vec3f {
    let uv = (vec2f(id.xy) + 0.5) / f32(size) * 2.0 - 1.0;
    return cube_dir(id.z, uv);
}

/*┌─────────────────╮
| Sources         |
└─────────────────╯*/
//region Sources

// A soft rectangular light seen from the centre, `half` its half-size in
// tangent units, with a smooth edge of width `soft`.
fn softbox(d: vec3f, center: vec3f, half: vec2f, soft: f32) -> f32 {
    let c = normalize(center);
    let right = normalize(cross(vec3f(0.0, 1.0, 0.0), c));
    let up = cross(c, right);
    let z = dot(d, c);
    if z <= 0.0 {
        return 0.0;
    }
    let q = abs(vec2f(dot(d, right), dot(d, up)) / z) / half;
    let e = pow(pow(q.x, 4.0) + pow(q.y, 4.0), 0.25);
    return 1.0 - smoothstep(1.0 - soft, 1.0 + soft, e);
}

// The default lighting: a photo studio with grey walls, a darker floor, a
// large key softbox to the upper left of the default camera, a fill on the
// right, a rim light behind and a strip overhead.  The key light's
// direction must match `studio_key_light` in ibl.rs.
fn studio(d: vec3f) -> vec3f {
    let y = d.y;
    var c = mix(vec3f(0.04, 0.04, 0.043), vec3f(0.155, 0.16, 0.175), smoothstep(-0.3, 0.3, y));
    c += vec3f(0.04, 0.041, 0.043) * smoothstep(0.3, 1.0, y);
    c += vec3f(1.0, 0.97, 0.93) * 2.3 * softbox(d, vec3f(-0.24, 0.71, 0.66), vec2f(0.45, 0.32), 0.35);
    c += vec3f(0.9, 0.94, 1.0) * 1.2 * softbox(d, vec3f(0.951, 0.259, -0.168), vec2f(0.8, 0.5), 0.4);
    c += vec3f(1.0) * 2.3 * softbox(d, vec3f(-0.280, 0.574, -0.770), vec2f(0.7, 0.18), 0.35);
    c += vec3f(1.0) * 0.65 * softbox(d, vec3f(0.0, 0.97, 0.24), vec2f(0.25, 0.9), 0.35);
    return c;
}

@compute @workgroup_size(8, 8, 1)
fn cs_studio(@builtin(global_invocation_id) id: vec3u) {
    let size = params.a.x;
    if id.x >= size || id.y >= size {
        return;
    }
    textureStore(dst, id.xy, id.z, vec4f(studio(texel_dir(id, size)), 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn cs_equirect(@builtin(global_invocation_id) id: vec3u) {
    let size = params.a.x;
    if id.x >= size || id.y >= size {
        return;
    }
    let d = texel_dir(id, size);
    let uv = vec2f(atan2(d.z, d.x) / (2.0 * PI) + 0.5, acos(clamp(d.y, -1.0, 1.0)) / PI);
    let c = textureSampleLevel(src_equirect, src_sampler, uv, params.b.y).rgb;
    textureStore(dst, id.xy, id.z, vec4f(max(c, vec3f(0.0)), 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn cs_from_cube(@builtin(global_invocation_id) id: vec3u) {
    let size = params.a.x;
    if id.x >= size || id.y >= size {
        return;
    }
    let c = textureSampleLevel(src_cube, src_sampler, texel_dir(id, size), params.b.y).rgb;
    textureStore(dst, id.xy, id.z, vec4f(max(c, vec3f(0.0)), 1.0));
}

// One mip level from the one above, by averaging 2x2 texels.
@compute @workgroup_size(8, 8, 1)
fn cs_downsample(@builtin(global_invocation_id) id: vec3u) {
    let size = params.a.x;
    if id.x >= size || id.y >= size {
        return;
    }
    let p = vec2i(id.xy) * 2;
    let f = i32(id.z);
    let c = textureLoad(src_level, p, f, 0) + textureLoad(src_level, p + vec2i(1, 0), f, 0)
        + textureLoad(src_level, p + vec2i(0, 1), f, 0) + textureLoad(src_level, p + vec2i(1, 1), f, 0);
    textureStore(dst, id.xy, id.z, c * 0.25);
}

//endregion

/*┌─────────────────╮
| Prefiltering    |
└─────────────────╯*/
//region Prefiltering

fn hammersley(i: u32, n: u32) -> vec2f {
    return vec2f(f32(i) / f32(n), f32(reverseBits(i)) * 2.3283064365386963e-10);
}

// A half vector around +Z distributed like GGX with roughness `a` (alpha).
fn importance_ggx(xi: vec2f, a: f32) -> vec3f {
    let phi = 2.0 * PI * xi.x;
    let cos_t = sqrt((1.0 - xi.y) / (1.0 + (a * a - 1.0) * xi.y));
    let sin_t = sqrt(max(1.0 - cos_t * cos_t, 0.0));
    return vec3f(sin_t * cos(phi), sin_t * sin(phi), cos_t);
}

fn d_ggx(n_dot_h: f32, a: f32) -> f32 {
    let a2 = a * a;
    let f = (n_dot_h * n_dot_h) * (a2 - 1.0) + 1.0;
    return a2 / (PI * f * f);
}

fn v_ggx(n_dot_l: f32, n_dot_v: f32, a: f32) -> f32 {
    let a2 = a * a;
    let ggxv = n_dot_l * sqrt(n_dot_v * n_dot_v * (1.0 - a2) + a2);
    let ggxl = n_dot_v * sqrt(n_dot_l * n_dot_l * (1.0 - a2) + a2);
    let ggx = ggxv + ggxl;
    return select(0.0, 0.5 / ggx, ggx > 0.0);
}

// One level of the radiance cube: the environment convolved with GGX for
// this level's roughness, with V = N.  Samples read lower-resolution mips
// of the source where the distribution is wide (filtered importance
// sampling), so few samples give a smooth result.
@compute @workgroup_size(8, 8, 1)
fn cs_prefilter(@builtin(global_invocation_id) id: vec3u) {
    let size = params.a.x;
    if id.x >= size || id.y >= size {
        return;
    }
    let n = texel_dir(id, size);
    let roughness = params.b.x;
    let src_size = params.b.z;
    let max_lod = params.b.w - 1.0;
    if roughness <= 0.0 {
        let lod = max(log2(src_size / f32(size)), 0.0);
        textureStore(dst, id.xy, id.z, vec4f(textureSampleLevel(src_cube, src_sampler, n, lod).rgb, 1.0));
        return;
    }
    let a = roughness * roughness;
    let up = select(vec3f(0.0, 0.0, 1.0), vec3f(1.0, 0.0, 0.0), abs(n.z) > 0.999);
    let tx = normalize(cross(up, n));
    let ty = cross(n, tx);
    let count = params.a.y;
    let omega_p = 4.0 * PI / (6.0 * src_size * src_size);
    var sum = vec3f(0.0);
    var weight = 0.0;
    for (var i = 0u; i < count; i++) {
        let ht = importance_ggx(hammersley(i, count), a);
        let h = tx * ht.x + ty * ht.y + n * ht.z;
        let l = 2.0 * dot(n, h) * h - n;
        let n_dot_l = dot(n, l);
        if n_dot_l > 0.0 {
            let pdf = d_ggx(ht.z, a) * 0.25;
            let omega_s = 1.0 / (f32(count) * pdf + 1e-6);
            let lod = clamp(0.5 * log2(omega_s / omega_p) + 1.0, 0.0, max_lod);
            sum += textureSampleLevel(src_cube, src_sampler, l, lod).rgb * n_dot_l;
            weight += n_dot_l;
        }
    }
    textureStore(dst, id.xy, id.z, vec4f(sum / max(weight, 1e-6), 1.0));
}

//endregion

/*┌─────────────────╮
| Irradiance      |
└─────────────────╯*/
//region Irradiance

var<workgroup> partial: array<array<vec3f, 9>, 64>;

// Projects a small mip of the environment onto nine spherical harmonics,
// weighting texels by solid angle, and stores them pre-scaled by the
// cosine lobe's convolution factors divided by pi, ready for lighting.
@compute @workgroup_size(64, 1, 1)
fn cs_sh(@builtin(local_invocation_index) li: u32) {
    let size = params.a.x;
    let total = size * size * 6u;
    var acc: array<vec3f, 9>;
    for (var k = 0u; k < 9u; k++) {
        acc[k] = vec3f(0.0);
    }
    var t = li;
    while t < total {
        let face = t / (size * size);
        let rest = t % (size * size);
        let xy = vec2u(rest % size, rest / size);
        let uv = (vec2f(xy) + 0.5) / f32(size) * 2.0 - 1.0;
        let d = cube_dir(face, uv);
        let w = 4.0 / (f32(size) * f32(size)) / pow(1.0 + dot(uv, uv), 1.5);
        let c = textureLoad(src_level, vec2i(xy), i32(face), 0).rgb * w;
        acc[0] += c * 0.282095;
        acc[1] += c * (0.488603 * d.y);
        acc[2] += c * (0.488603 * d.z);
        acc[3] += c * (0.488603 * d.x);
        acc[4] += c * (1.092548 * d.x * d.y);
        acc[5] += c * (1.092548 * d.y * d.z);
        acc[6] += c * (0.315392 * (3.0 * d.z * d.z - 1.0));
        acc[7] += c * (1.092548 * d.x * d.z);
        acc[8] += c * (0.546274 * (d.x * d.x - d.y * d.y));
        t += 64u;
    }
    for (var k = 0u; k < 9u; k++) {
        partial[li][k] = acc[k];
    }
    workgroupBarrier();
    if li == 0u {
        for (var k = 0u; k < 9u; k++) {
            var s = vec3f(0.0);
            for (var j = 0u; j < 64u; j++) {
                s += partial[j][k];
            }
            sh_out[k] = vec4f(s, 0.0);
        }
    }
}

//endregion
