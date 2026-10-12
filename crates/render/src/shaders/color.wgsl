// Colour encoding and tone mapping, shared by every shader module.

const PI: f32 = 3.14159265359;

fn srgb_to_linear(c: vec3f) -> vec3f {
    let lo = c / 12.92;
    let hi = pow((c + 0.055) / 1.055, vec3f(2.4));
    return select(hi, lo, c <= vec3f(0.04045));
}

fn linear_to_srgb(c: vec3f) -> vec3f {
    let lo = c * 12.92;
    let hi = 1.055 * pow(max(c, vec3f(0.0)), vec3f(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3f(0.0031308));
}

// Khronos PBR Neutral: leaves colours below about 0.76 untouched, so base
// colours show as authored, and compresses highlights.
fn tonemap_neutral(c_in: vec3f) -> vec3f {
    let start = 0.8 - 0.04;
    let desaturation = 0.15;
    var c = c_in;
    let x = min(c.r, min(c.g, c.b));
    let offset = select(0.04, x - 6.25 * x * x, x < 0.08);
    c -= offset;
    let peak = max(c.r, max(c.g, c.b));
    if peak < start {
        return c;
    }
    let d = 1.0 - start;
    let new_peak = 1.0 - d * d / (peak + d - start);
    c *= new_peak / peak;
    let g = 1.0 - 1.0 / (desaturation * (peak - new_peak) + 1.0);
    return mix(c, vec3f(new_peak), g);
}

// Stephen Hill's fit of the ACES reference transforms, as in the Khronos
// sample viewer.
fn tonemap_aces(c_in: vec3f) -> vec3f {
    let input = mat3x3f(0.59719, 0.07600, 0.02840, 0.35458, 0.90834, 0.13383, 0.04823, 0.01566, 0.83777);
    let output = mat3x3f(1.60475, -0.10208, -0.00327, -0.53108, 1.10813, -0.07276, -0.07367, -0.00605, 1.07602);
    var c = input * (c_in / 0.6);
    let a = c * (c + 0.0245786) - 0.000090537;
    let b = c * (0.983729 * c + 0.4329510) + 0.238081;
    c = output * (a / b);
    return clamp(c, vec3f(0.0), vec3f(1.0));
}

// Tone map `c` (already exposed): 0 clamps, 1 is PBR Neutral, 2 is ACES,
// and 3 leaves scene-referred light as it is, for the texture that
// transmissive surfaces see through.
fn tonemap_mode(c: vec3f, mode: u32) -> vec3f {
    if mode == 1u {
        return tonemap_neutral(c);
    }
    if mode == 2u {
        return tonemap_aces(c);
    }
    if mode == 3u {
        return max(c, vec3f(0.0));
    }
    return clamp(c, vec3f(0.0), vec3f(1.0));
}

// A full-screen triangle's clip position for vertex 0, 1 or 2.
fn fullscreen(i: u32) -> vec4f {
    let uv = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4f(uv * 2.0 - 1.0, 0.0, 1.0);
}
