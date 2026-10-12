// Image-based lighting, the key light's shadow map and the ground's contact
// shadow map, bound only in the main pass (the shadow passes render into
// two of these textures).

@group(3) @binding(0) var radiance: texture_cube<f32>;
@group(3) @binding(1) var env_sampler: sampler;
@group(3) @binding(2) var brdf_lut: texture_2d<f32>;
// Irradiance as nine spherical-harmonic coefficients (rgb), already
// convolved with the cosine lobe and divided by pi.
@group(3) @binding(3) var<uniform> sh: array<vec4f, 9>;
@group(3) @binding(4) var shadow_map: texture_depth_2d;
@group(3) @binding(5) var shadow_sampler: sampler_comparison;
@group(3) @binding(6) var contact_map: texture_2d<f32>;
@group(3) @binding(7) var clamp_sampler: sampler;
// The environment at its full resolution, for an unblurred background.
@group(3) @binding(8) var sharp_env: texture_cube<f32>;
// Screen-space ambient occlusion: visibility, the view depth it was
// computed at, and the visibility of reflections, per pixel (ao.wgsl).
@group(3) @binding(9) var ao_map: texture_2d<f32>;
// The opaque scene in linear light with mips, premultiplied alpha, which
// transmissive surfaces see through.
@group(3) @binding(10) var transmission_map: texture_2d<f32>;

const AO_TAPS = array<vec2i, 5>(vec2i(0, 0), vec2i(1, 0), vec2i(-1, 0), vec2i(0, 1), vec2i(0, -1));

// How much of the environment's light reaches the surface at pixel
// `frag` and point `world`, from the ambient occlusion pass, raised to its
// strength: 1 when the pass didn't run.  At silhouettes the pixel's own
// value may belong to the surface in front (the pass has one sample per
// pixel, the main pass four), so the neighbour whose depth matches best
// stands in, and none at all means no occlusion.  `y` is how much of the
// reflection along the mirror direction escapes.
fn screen_ao(frag: vec2f, world: vec3f) -> vec2f {
    let both = screen_ao_texel(frag, world);
    return pow(both.xz, vec2f(frame.ao.x));
}

// The ambient occlusion pass's texel for pixel `frag` and point `world`:
// visibility, depth, reflection visibility and point-cloud shade, all 1
// (depth 0) when the pass didn't run or saw another surface there.
fn screen_ao_texel(frag: vec2f, world: vec3f) -> vec4f {
    if frame.ao.x <= 0.0 {
        return vec4f(1.0, 0.0, 1.0, 1.0);
    }
    let depth = -(frame.view * vec4f(world, 1.0)).z;
    let size = vec2i(textureDimensions(ao_map));
    let p = vec2i(frag);
    var best = vec4f(1.0, 0.0, 1.0, 1.0);
    var err = depth * 0.02;
    for (var i = 0; i < 5; i++) {
        let t = textureLoad(ao_map, clamp(p + AO_TAPS[i], vec2i(0), size - 1), 0);
        let e = abs(t.y - depth);
        if e < err {
            err = e;
            best = t;
        }
    }
    return best;
}

// Diffuse light arriving from the environment around normal `n`, divided
// by pi, so that albedo times this is the reflected radiance.
fn irradiance(n_world: vec3f) -> vec3f {
    let n = env_dir(n_world);
    var c = sh[0].rgb * 0.282095;
    c += (sh[1].rgb * n.y + sh[2].rgb * n.z + sh[3].rgb * n.x) * 0.488603 * (2.0 / 3.0);
    c += (sh[4].rgb * (1.092548 * n.x * n.y) + sh[5].rgb * (1.092548 * n.y * n.z)
        + sh[6].rgb * (0.315392 * (3.0 * n.z * n.z - 1.0)) + sh[7].rgb * (1.092548 * n.x * n.z)
        + sh[8].rgb * (0.546274 * (n.x * n.x - n.y * n.y))) * 0.25;
    return max(c, vec3f(0.0)) * frame.env.z;
}

// Prefiltered specular light along `r` for a perceptual roughness.
fn specular_radiance(r: vec3f, roughness: f32) -> vec3f {
    let lod = roughness * frame.env.w;
    return textureSampleLevel(radiance, env_sampler, env_dir(r), lod).rgb * frame.env.z;
}

// The split-sum scale and bias for F0, from the precomputed table.
fn brdf_terms(n_dot_v: f32, roughness: f32) -> vec2f {
    let uv = clamp(vec2f(n_dot_v, roughness), vec2f(0.0), vec2f(1.0));
    return textureSampleLevel(brdf_lut, clamp_sampler, uv, 0.0).rg;
}

// The fraction of key light reaching `world`, softened by a 3x3 filter of
// hardware-compared samples.  `soft` widens the filter, in texels.
fn shadow(world: vec3f, n: vec3f, soft: f32) -> f32 {
    let strength = frame.light_dir.w;
    if strength <= 0.0 {
        return 1.0;
    }
    let texel = frame.light_color.w;
    // Offsetting along the normal avoids acne without a large depth bias.
    let p = frame.light_view_proj * vec4f(world + n * frame.shadow.x, 1.0);
    let uv = p.xy * vec2f(0.5, -0.5) + 0.5;
    if any(uv < vec2f(0.0)) || any(uv > vec2f(1.0)) || p.z > 1.0 {
        return 1.0;
    }
    let depth = p.z - frame.shadow.y;
    var sum = 0.0;
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let o = vec2f(f32(x), f32(y)) * texel * soft;
            sum += textureSampleCompareLevel(shadow_map, shadow_sampler, uv + o, depth);
        }
    }
    return mix(1.0, sum / 9.0, strength);
}
