// The viewer's texture mode: one mip, layer or face of a texture, panned
// and zoomed, with a channel mask, exposure and tone mapping for HDR, and a
// checkerboard behind transparent texels.  Prefixed with color.wgsl.

struct TexParams {
    // Screen pixel to level-0 texel: texel = (pixel - xy) * z.
    xform: vec4f,
    // Level-0 width and height, level of detail, array layer.
    tex: vec4f,
    // Exposure multiplier, tone map mode, 1 for an sRGB target, 1 for HDR.
    color: vec4f,
    // Channel mask: 1 shows the channel.
    mask: vec4f,
    // Nearest filtering, checkerboard, single channel shown as grey (index
    // plus one, 0 for none), 0.
    flags: vec4u,
    // Colour around the texture, linear.
    background: vec4f,
}

@group(0) @binding(0) var<uniform> p: TexParams;
@group(0) @binding(1) var tex: texture_2d_array<f32>;
@group(0) @binding(2) var nearest: sampler;
@group(0) @binding(3) var linear: sampler;

@vertex
fn vs_texview(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    return fullscreen(i);
}

fn encode(display: vec3f) -> vec3f {
    if p.color.z > 0.5 {
        return display;
    }
    return linear_to_srgb(display);
}

@fragment
fn fs_texview(@builtin(position) pos: vec4f) -> @location(0) vec4f {
    let texel = (pos.xy - p.xform.xy) * p.xform.z;
    let uv = texel / p.tex.xy;
    let layer = i32(p.tex.w);
    let a = textureSampleLevel(tex, nearest, uv, layer, p.tex.z);
    let b = textureSampleLevel(tex, linear, uv, layer, p.tex.z);
    if any(uv < vec2f(0.0)) || any(uv > vec2f(1.0)) {
        return vec4f(encode(p.background.rgb), 1.0);
    }
    var c = select(b, a, p.flags.x == 1u);
    if p.flags.z != 0u {
        let v = c[p.flags.z - 1u];
        c = vec4f(v, v, v, 1.0);
    } else {
        c = vec4f(c.rgb * p.mask.rgb, select(1.0, c.a, p.mask.a > 0.5));
    }
    var rgb = c.rgb;
    if p.color.w > 0.5 {
        rgb = tonemap_mode(rgb * p.color.x, u32(p.color.y + 0.5));
    }
    var alpha = clamp(c.a, 0.0, 1.0);
    if p.flags.y == 1u {
        let cell = floor(pos.xy / 8.0);
        let even = (i32(cell.x) + i32(cell.y)) % 2 == 0;
        let checker = srgb_to_linear(vec3f(select(0.6, 0.8, even)));
        rgb = mix(checker, rgb, alpha);
        alpha = 1.0;
    }
    return vec4f(encode(rgb) * alpha, alpha);
}
