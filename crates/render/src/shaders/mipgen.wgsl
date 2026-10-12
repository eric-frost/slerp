// Mipmap generation by rendering each level from the one above.  Prefixed
// with color.wgsl.

@group(0) @binding(0) var mip_src: texture_2d<f32>;
@group(0) @binding(1) var mip_sampler: sampler;

struct MipOut {
    @builtin(position) clip: vec4f,
    @location(0) uv: vec2f,
}

@vertex
fn vs_mip(@builtin(vertex_index) i: u32) -> MipOut {
    var o: MipOut;
    o.clip = fullscreen(i);
    o.uv = o.clip.xy * vec2f(0.5, -0.5) + 0.5;
    return o;
}

// Bilinear sampling half way between four texels averages them.
@fragment
fn fs_mip(in: MipOut) -> @location(0) vec4f {
    return textureSampleLevel(mip_src, mip_sampler, in.uv, 0.0);
}
