// Gaussian splats, step 4: lay the splats' layer over the main pass.  The
// layer holds display-encoded colour with premultiplied alpha; it is
// decoded to linear light (with the exposure applied) and written the way
// every scene shader writes, so the target's encoding and blending apply.
// Splats are captured from photos and already look as they should, so they
// aren't tone mapped.

@group(1) @binding(0) var splat_layer: texture_2d<f32>;

@vertex
fn vs_splat_composite(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    return fullscreen(i);
}

@fragment
fn fs_splat_composite(@builtin(position) at: vec4f) -> @location(0) vec4f {
    let s = textureLoad(splat_layer, vec2i(at.xy), 0);
    if s.a <= 0.0 {
        discard;
    }
    let a = min(s.a, 1.0);
    let display = clamp(s.rgb / s.a, vec3f(0.0), vec3f(1.0));
    let linear = min(srgb_to_linear(display) * frame.tone.x, vec3f(1.0));
    return vec4f(to_target(linear) * a, a);
}
