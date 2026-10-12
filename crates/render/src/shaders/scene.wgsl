// Materials, the PBR surface shader, the shadow passes, and every overlay
// that draws scene geometry.  Prefixed with common.wgsl and lighting.wgsl.
//
// The shading follows the Khronos glTF Sample Renderer (Apache-2.0):
// metallic-roughness with KHR_materials_ior, _specular, _clearcoat,
// _transmission and _volume, split-sum image-based lighting with
// multiple-scattering compensation, and one punctual key light.  Screen-
// space ambient occlusion (ao.wgsl) darkens the environment's light.

/*┌─────────────────╮
| Materials       |
└─────────────────╯*/
//region Materials

// A texture slot's transform (KHR_texture_transform) as two rows applied
// to (u, v, 1).  row0.w is the UV set; row1.w is 1 when the slot has a
// texture.
struct TexSlot {
    row0: vec4f,
    row1: vec4f,
}

struct Material {
    base_color: vec4f,
    emissive: vec4f,
    // rgb specular colour, w specular factor.
    specular: vec4f,
    // metallic, roughness, normal scale, occlusion strength.
    pbr: vec4f,
    // alpha cutoff, ior, clearcoat, clearcoat roughness.
    extra: vec4f,
    // lightmap scale, height scale, debug-only flags, 0.
    extra2: vec4f,
    // alpha mode (0 opaque, 1 mask, 2 blend), unlit, effect (0 none,
    // 1 warp, 2 Quake sky, 3 panorama sky, 4 billboard), double-sided.
    flags: vec4u,
    // transmission, thickness (mesh units), attenuation distance (0 for
    // none), 0.
    transmission: vec4f,
    // attenuation colour (linear), 0.
    attenuation: vec4f,
    // distance fog: colour (linear), density per metre (0 for none).
    fog: vec4f,
    // brightening near the camera: factor at the camera, distance (metres,
    // 0 for none) where it ends, 1 when shaded as Doom does, 0.
    fog_near: vec4f,
    slots: array<TexSlot, 9>,
}

const SLOT_BASE: u32 = 0u;
const SLOT_MR: u32 = 1u;
const SLOT_NORMAL: u32 = 2u;
const SLOT_OCCLUSION: u32 = 3u;
const SLOT_EMISSIVE: u32 = 4u;
const SLOT_LIGHTMAP: u32 = 5u;
const SLOT_HEIGHT: u32 = 6u;
const SLOT_TRANSMISSION: u32 = 7u;
const SLOT_THICKNESS: u32 = 8u;

@group(1) @binding(0) var<uniform> material: Material;
@group(1) @binding(1) var t_base: texture_2d<f32>;
@group(1) @binding(2) var t_mr: texture_2d<f32>;
@group(1) @binding(3) var t_normal: texture_2d<f32>;
@group(1) @binding(4) var t_occlusion: texture_2d<f32>;
@group(1) @binding(5) var t_emissive: texture_2d<f32>;
@group(1) @binding(6) var t_lightmap: texture_2d<f32>;
@group(1) @binding(7) var t_height: texture_2d<f32>;
@group(1) @binding(8) var t_transmission: texture_2d<f32>;
@group(1) @binding(9) var t_thickness: texture_2d<f32>;
@group(1) @binding(10) var s_base: sampler;
@group(1) @binding(11) var s_mr: sampler;
@group(1) @binding(12) var s_normal: sampler;
@group(1) @binding(13) var s_occlusion: sampler;
@group(1) @binding(14) var s_emissive: sampler;
@group(1) @binding(15) var s_lightmap: sampler;
@group(1) @binding(16) var s_height: sampler;
@group(1) @binding(17) var s_transmission: sampler;
@group(1) @binding(18) var s_thickness: sampler;

fn has_slot(i: u32) -> bool {
    return material.slots[i].row1.w > 0.5;
}

fn slot_uv(i: u32, uv0: vec2f, uv1: vec2f) -> vec2f {
    let s = material.slots[i];
    let uv = select(uv0, uv1, s.row0.w > 0.5);
    return vec2f(dot(s.row0.xyz, vec3f(uv, 1.0)), dot(s.row1.xyz, vec3f(uv, 1.0)));
}

//endregion

/*┌─────────────────╮
| Vertex stage    |
└─────────────────╯*/
//region Vertex stage

struct VOut {
    @builtin(position) clip: vec4f,
    @location(0) world: vec3f,
    @location(1) normal: vec3f,
    @location(2) tangent: vec4f,
    @location(3) uv0: vec2f,
    @location(4) uv1: vec2f,
    @location(5) color: vec4f,
    // Barycentric coordinates for the wireframe; for points, where the
    // fragment lies on the dot (the unit disc) and the point spacing.
    @location(6) bary: vec3f,
    // The model matrix's scale along its axes, which turns a volume's
    // thickness into world units.
    @location(7) @interpolate(flat) scale: vec3f,
}

fn transform(d: Draw, vtx_in: Vertex) -> VOut {
    var vtx = vtx_in;
    // Displacement for material balls, along the normal.
    if has_slot(SLOT_HEIGHT) && material.extra2.y != 0.0 && dot(vtx.normal, vtx.normal) > 0.0 {
        let h = textureSampleLevel(t_height, s_height, slot_uv(SLOT_HEIGHT, vtx.uv0, vtx.uv1), 0.0).r;
        vtx.pos += normalize(vtx.normal) * h * material.extra2.y;
    }
    var o: VOut;
    o.world = (d.model * vec4f(vtx.pos, 1.0)).xyz;
    o.normal = (d.normal * vec4f(vtx.normal, 0.0)).xyz;
    // Billboards: the vertex is its sprite's anchor and uv1 the corner's
    // offset, right and up, turned about +Y to face the camera.
    if material.flags.z == 4u {
        var toward = frame.camera.xyz - o.world;
        if frame.camera.w > 0.5 {
            toward = -frame.camera_forward.xyz;
        }
        let flat = select(vec2f(0.0, 1.0), normalize(toward.xz), dot(toward.xz, toward.xz) > 1e-12);
        o.world += vec3f(flat.y, 0.0, -flat.x) * vtx.uv1.x + vec3f(0.0, vtx.uv1.y, 0.0);
        o.normal = vec3f(flat.x, 0.0, flat.y);
    }
    o.clip = frame.view_proj * vec4f(o.world, 1.0);
    o.tangent = vec4f((d.model * vec4f(vtx.tangent.xyz, 0.0)).xyz, vtx.tangent.w);
    o.uv0 = vtx.uv0;
    o.uv1 = vtx.uv1;
    o.color = vtx.color;
    o.bary = vec3f(0.0);
    o.scale = vec3f(length(d.model[0].xyz), length(d.model[1].xyz), length(d.model[2].xyz));
    return o;
}

@vertex
fn vs_main(@builtin(vertex_index) v: u32, @builtin(instance_index) inst: u32) -> VOut {
    let d = instance_draw(inst);
    return transform(d, fetch_vertex(d, v));
}

// Draws every triangle corner separately, looking the vertex up in the
// index data, so each corner gets its own barycentric coordinate.
@vertex
fn vs_wire(@builtin(vertex_index) corner: u32, @builtin(instance_index) inst: u32) -> VOut {
    let d = instance_draw(inst);
    var o = transform(d, fetch_vertex(d, indexed_vertex(d, corner)));
    let k = corner % 3u;
    o.bary = vec3f(f32(k == 0u), f32(k == 1u), f32(k == 2u));
    return o;
}

//endregion

/*┌─────────────────╮
| Shadow passes   |
└─────────────────╯*/
//region Shadow passes

fn alpha_test(in: VOut) {
    if material.flags.x == 1u {
        let uv = slot_uv(SLOT_BASE, in.uv0, in.uv1);
        let a = textureSample(t_base, s_base, uv).a * material.base_color.a * in.color.a;
        if a < material.extra.x {
            discard;
        }
    }
}

@fragment
fn fs_shadow(in: VOut) {
    alpha_test(in);
}

// Seen from below the ground: how strongly each spot of ground is covered,
// fading with the height of whatever covers it.
@fragment
fn fs_contact(in: VOut) -> @location(0) vec4f {
    alpha_test(in);
    let h = clamp((in.world.y - frame.ground.x) / max(frame.ground.y, 1e-6), 0.0, 1.0);
    let o = 1.0 - h;
    return vec4f(o * o, 0.0, 0.0, 1.0);
}

// The ambient occlusion pre-pass: depth, and the normal in view space,
// turned towards the camera on the back faces of double-sided surfaces.
// Alpha is the distance within which neighbours don't occlude: 0 for
// surfaces.
@fragment
fn fs_prepass(in: VOut, @builtin(front_facing) front: bool) -> @location(0) vec4f {
    let face = cross(dpdx(in.world), dpdy(in.world));
    alpha_test(in);
    var n = in.normal;
    if dot(n, n) < 1e-12 {
        n = select(vec3f(0.0, 1.0, 0.0), face, dot(face, face) > 1e-20);
        n *= sign(dot(n, frame.camera.xyz - in.world) + 1e-20);
    } else if !front {
        n = -n;
    }
    return vec4f(normalize((frame.view * vec4f(normalize(n), 0.0)).xyz), 0.0);
}

//endregion

/*┌─────────────────╮
| Surface         |
└─────────────────╯*/
//region Surface

fn d_ggx(n_dot_h: f32, a: f32) -> f32 {
    let a2 = a * a;
    let f = (n_dot_h * n_dot_h) * (a2 - 1.0) + 1.0;
    return a2 / (PI * f * f);
}

// Height-correlated Smith visibility, including 1 / (4 N.L N.V).
fn v_ggx(n_dot_l: f32, n_dot_v: f32, a: f32) -> f32 {
    let a2 = a * a;
    let ggxv = n_dot_l * sqrt(n_dot_v * n_dot_v * (1.0 - a2) + a2);
    let ggxl = n_dot_v * sqrt(n_dot_l * n_dot_l * (1.0 - a2) + a2);
    let ggx = ggxv + ggxl;
    return select(0.0, 0.5 / ggx, ggx > 0.0);
}

fn f_schlick(f0: vec3f, f90: vec3f, v_dot_h: f32) -> vec3f {
    return f0 + (f90 - f0) * pow(clamp(1.0 - v_dot_h, 0.0, 1.0), 5.0);
}

// Fresnel-weighted split-sum reflectance with Fdez-Aguera's multiple-
// scattering term, so rough metals don't lose energy.
fn ggx_fresnel(lut: vec2f, n_dot_v: f32, roughness: f32, f0: vec3f, weight: f32) -> vec3f {
    let fr = max(vec3f(1.0 - roughness), f0) - f0;
    let k_s = f0 + fr * pow(1.0 - n_dot_v, 5.0);
    let fss_ess = weight * (k_s * lut.x + lut.y);
    let ems = 1.0 - (lut.x + lut.y);
    let f_avg = weight * (f0 + (1.0 - f0) / 21.0);
    let fms_ems = ems * fss_ess * f_avg / (1.0 - f_avg * ems);
    return fss_ess + fms_ems;
}

// What the main pass writes: display colour in the target's encoding with
// premultiplied alpha.
fn finish(display: vec3f, alpha: f32) -> vec4f {
    return vec4f(to_target(display) * alpha, alpha);
}

fn finish_raw(c: vec3f) -> vec4f {
    return vec4f(raw_to_target(c), 1.0);
}

// Quake's sky: two layers scrolling at different speeds over a flattened
// dome, the front layer (left half) transparent where black.  Without its
// texture (a map whose WAD is missing), the environment shows instead.
fn sky(in: VOut) -> vec3f {
    let view_dir = normalize(in.world - frame.camera.xyz);
    let env = textureSampleLevel(sharp_env, env_sampler, env_dir(view_dir), 0.0).rgb * frame.env.z;
    var dir = in.world - frame.camera.xyz;
    dir.y *= 3.0;
    let st = dir.xz * (378.0 / max(length(dir), 1e-6));
    let t = frame.misc.x;
    let back = (st + t * 8.0) / 128.0;
    let front = (st + t * 16.0) / 128.0;
    // Gradients of the unwrapped coordinates, so wrapping into each half
    // of the texture doesn't pick the smallest mip along the seams.
    let ddx = vec2f(dpdx(back.x) * 0.5, dpdx(back.y));
    let ddy = vec2f(dpdy(back.x) * 0.5, dpdy(back.y));
    let back_uv = vec2f(fract(back.x) * 0.5 + 0.5, fract(back.y));
    let front_uv = vec2f(fract(front.x) * 0.5, fract(front.y));
    let b = textureSampleGrad(t_base, s_base, back_uv, ddx, ddy);
    let f = textureSampleGrad(t_base, s_base, front_uv, ddx, ddy);
    let fa = select(f.a, 0.0, max(f.r, max(f.g, f.b)) < 0.004);
    return select(env, mix(b.rgb, f.rgb, fa), has_slot(SLOT_BASE));
}

// Doom's sky: the texture wrapped around the viewer as a cylinder, u the
// turns anticlockwise from +X seen from above and v the tangent of the
// elevation, both through the base slot's transform, which places the
// horizon and the repeats.  The turn count jumps at its seam, so the
// gradients come from whichever of it and its wrapped copy is smooth
// there.  Without its texture the environment shows.
fn panorama(in: VOut) -> vec3f {
    let d = in.world - frame.camera.xyz;
    let flat = max(length(d.xz), 1e-6);
    let turns = atan2(-d.z, d.x) / (2.0 * PI);
    let st = vec2f(turns, d.y / flat);
    let uv = slot_uv(SLOT_BASE, st, in.uv1);
    let wrapped = slot_uv(SLOT_BASE, vec2f(fract(turns), st.y), in.uv1);
    var ddx = dpdx(uv);
    var ddy = dpdy(uv);
    let wx = dpdx(wrapped);
    let wy = dpdy(wrapped);
    if dot(wx, wx) + dot(wy, wy) < dot(ddx, ddx) + dot(ddy, ddy) {
        ddx = wx;
        ddy = wy;
    }
    if !has_slot(SLOT_BASE) {
        return textureSampleLevel(sharp_env, env_sampler, env_dir(normalize(d)), 0.0).rgb * frame.env.z;
    }
    return textureSampleGrad(t_base, s_base, uv, ddx, ddy).rgb * material.base_color.rgb;
}

@fragment
fn fs_main(in: VOut, @builtin(front_facing) front: bool) -> @location(0) vec4f {
    // 1. UVs, with Quake's liquid turbulence.
    var uv0 = in.uv0;
    if material.flags.z == 1u {
        let size = vec2f(textureDimensions(t_base));
        uv0 += (8.0 / size) * sin(uv0.yx * size.yx * 0.125 + frame.misc.x);
    }
    let uv_base = slot_uv(SLOT_BASE, uv0, in.uv1);
    let uv_normal = slot_uv(SLOT_NORMAL, uv0, in.uv1);

    // 2. Everything that needs derivatives or implicit-lod sampling, while
    //    control flow is still uniform.
    let dp_dx = dpdx(in.world);
    let dp_dy = dpdy(in.world);
    let duv_dx = dpdx(uv_normal);
    let duv_dy = dpdy(uv_normal);
    let tex_base = textureSample(t_base, s_base, uv_base);
    let tex_mr = textureSample(t_mr, s_mr, slot_uv(SLOT_MR, uv0, in.uv1));
    let tex_normal = textureSample(t_normal, s_normal, uv_normal);
    let tex_occlusion = textureSample(t_occlusion, s_occlusion, slot_uv(SLOT_OCCLUSION, uv0, in.uv1));
    let tex_emissive = textureSample(t_emissive, s_emissive, slot_uv(SLOT_EMISSIVE, uv0, in.uv1));
    let tex_lightmap = textureSample(t_lightmap, s_lightmap, slot_uv(SLOT_LIGHTMAP, uv0, in.uv1));
    let tex_transmission = textureSample(t_transmission, s_transmission, slot_uv(SLOT_TRANSMISSION, uv0, in.uv1));
    let tex_thickness = textureSample(t_thickness, s_thickness, slot_uv(SLOT_THICKNESS, uv0, in.uv1));
    let sky_color = sky(in);
    var panorama_color = vec3f(0.0);
    if material.flags.z == 3u {
        panorama_color = panorama(in);
    }

    // 3. Base colour and alpha.
    var base = material.base_color * in.color * tex_base;
    let alpha_mode = material.flags.x;
    if alpha_mode == 1u {
        if base.a < material.extra.x {
            discard;
        }
        base.a = 1.0;
    } else if alpha_mode == 0u {
        base.a = 1.0;
    }

    // 4. View direction and the geometric normal; primitives without
    //    normals use the face normal, or face the viewer (lines, points).
    var v = normalize(frame.camera.xyz - in.world);
    if frame.camera.w > 0.5 {
        v = -frame.camera_forward.xyz;
    }
    let has_normal = dot(in.normal, in.normal) > 1e-12;
    let face = cross(dp_dx, dp_dy);
    let displaced = has_slot(SLOT_HEIGHT) && material.extra2.y != 0.0;
    var ng: vec3f;
    if has_normal && displaced && dot(face, face) > 1e-20 {
        // Displacement moved the surface but not its normals; the face
        // normal of the displaced triangles shows the relief.
        ng = normalize(face) * sign(dot(face, in.normal));
    } else if has_normal {
        ng = normalize(in.normal);
    } else {
        ng = select(v, normalize(face), dot(face, face) > 1e-20);
        if dot(ng, v) < 0.0 {
            ng = -ng;
        }
    }

    // 5. Tangent frame: from the mesh's tangents, else from screen-space
    //    derivatives of position and UV.  Back faces of double-sided
    //    materials flip the whole frame.
    var t: vec3f;
    var b: vec3f;
    if dot(in.tangent.xyz, in.tangent.xyz) > 1e-12 {
        t = in.tangent.xyz - ng * dot(ng, in.tangent.xyz);
        t = select(normalize(cross(ng, vec3f(0.0, 1.0, 0.1))), normalize(t), dot(t, t) > 1e-20);
        b = cross(ng, t) * select(1.0, -1.0, in.tangent.w < 0.0);
    } else {
        var ux = vec3f(duv_dx, 0.0);
        var uy = vec3f(duv_dy, 0.0);
        if length(ux) <= 1e-6 {
            ux = vec3f(1.0, 0.0, 0.0);
        }
        if length(uy) <= 1e-6 {
            uy = vec3f(0.0, 1.0, 0.0);
        }
        let det = ux.x * uy.y - uy.x * ux.y;
        let t_ = (uy.y * dp_dx - ux.y * dp_dy) / select(det, 1e-6, abs(det) < 1e-12);
        t = t_ - ng * dot(ng, t_);
        t = select(normalize(cross(ng, vec3f(0.0, 1.0, 0.1))), normalize(t), dot(t, t) > 1e-20);
        b = cross(ng, t);
    }
    if has_normal && !front {
        t = -t;
        b = -b;
        ng = -ng;
    }
    var n = ng;
    if has_slot(SLOT_NORMAL) {
        var tn = tex_normal.rgb * 2.0 - 1.0;
        tn = vec3f(tn.xy * material.pbr.z, tn.z);
        n = normalize(mat3x3f(t, b, ng) * tn);
    }

    // 6. Material parameters.
    let metallic = clamp(material.pbr.x * tex_mr.b, 0.0, 1.0);
    let roughness = clamp(material.pbr.y * tex_mr.g, 0.0, 1.0);
    var occlusion = 1.0;
    if has_slot(SLOT_OCCLUSION) {
        occlusion = 1.0 + material.pbr.w * (tex_occlusion.r - 1.0);
    }
    let emissive = material.emissive.rgb * tex_emissive.rgb;

    // 7. Debug views of single channels.
    let debug = u32(frame.misc.z + 0.5);
    if debug != 0u {
        switch debug {
            case 1u: { return finish_raw(linear_to_srgb(base.rgb)); }
            case 2u: { return finish_raw(n * 0.5 + 0.5); }
            case 3u: { return finish_raw(vec3f(metallic)); }
            case 4u: { return finish_raw(vec3f(roughness)); }
            // The occlusion the lighting applies: the material's map, and
            // ambient occlusion when it is on.
            case 5u: { return finish_raw(vec3f(occlusion * screen_ao(in.clip.xy, in.world).x)); }
            case 6u: { return finish_raw(linear_to_srgb(emissive)); }
            default: {
                let cell = floor(in.uv0 * 8.0);
                let even = (i32(cell.x) + i32(cell.y)) % 2 == 0;
                let tint = vec3f(fract(in.uv0), 0.5);
                return finish_raw(mix(tint * 0.5, vec3f(0.95), select(0.15, 0.85, even)));
            }
        }
    }

    // 8. Surfaces that ignore scene lighting: unlit materials, Quake's sky
    //    and liquids, and lightmapped surfaces.
    let effect = material.flags.z;
    if effect == 2u {
        return finish(tonemap(sky_color), 1.0);
    }
    if effect == 3u {
        return finish(tonemap(panorama_color), 1.0);
    }
    if material.flags.y != 0u || effect == 1u {
        return finish(fogged(tonemap(base.rgb + emissive), in.world), base.a);
    }
    if has_slot(SLOT_LIGHTMAP) {
        // Lightmaps hold display-encoded light that Quake multiplies in
        // display space, overbright included; decoding the product keeps
        // that look.
        let light = pow(max(tex_lightmap.rgb * material.extra2.x, vec3f(0.0)), vec3f(2.2));
        return finish(tonemap(base.rgb * light + emissive), base.a);
    }

    // 9. Image-based lighting, darkened by ambient occlusion on top of the
    //    material's own occlusion map (MetaHumans' holds a specular mask).
    //    Blended and transmissive surfaces don't take part, nor do alpha-
    //    masked ones such as hair cards, whose strands would speckle.
    let n_dot_v = clamp(dot(n, v), 0.0, 1.0);
    let ior = material.extra.y;
    let f0_ior = pow((ior - 1.0) / (ior + 1.0), 2.0);
    let f0_d = min(vec3f(f0_ior) * material.specular.rgb, vec3f(1.0));
    let spec_w = material.specular.w;
    let lut = brdf_terms(n_dot_v, roughness);
    let transmission = clamp(material.transmission.x * tex_transmission.r, 0.0, 1.0);
    var screen_occlusion = vec2f(1.0);
    if alpha_mode == 0u && transmission <= 0.0 {
        screen_occlusion = screen_ao(in.clip.xy, in.world);
    }
    var f_diffuse = irradiance(n) * base.rgb;
    var spec_occlusion = 1.0;
    if screen_occlusion.x < 1.0 || screen_occlusion.y < 1.0 {
        f_diffuse *= screen_occlusion.x;
        spec_occlusion = specular_occlusion(n_dot_v, screen_occlusion, roughness);
    }
    let f_spec = specular_radiance(reflect(-v, n), roughness) * spec_occlusion;

    // 9a. Transmission, as the Khronos sample renderer does it: the light
    //     behind comes from the opaque scene drawn beforehand, where the
    //     ray refracted by the volume's thickness leaves it, blurred by
    //     roughness through the texture's mips and absorbed along the way
    //     by Beer's law.  It replaces the diffuse part.  Where nothing lies
    //     behind (a transparent thumbnail background) the glass shows the
    //     lighting environment, as the viewer's background would: PNG's
    //     alpha can't filter what is behind a picture by colour.
    var ray = vec3f(0.0);
    if transmission > 0.0 {
        let thickness = material.transmission.y * tex_thickness.g;
        let refracted = refract(-v, n, 1.0 / ior);
        let bent = select(-v, normalize(refracted), dot(refracted, refracted) > 1e-12);
        ray = bent * thickness * in.scale;
        let exit = frame.view_proj * vec4f(in.world + ray, 1.0);
        let uv = exit.xy / exit.w * vec2f(0.5, -0.5) + 0.5;
        let blur = roughness * clamp(ior * 2.0 - 2.0, 0.0, 1.0);
        var behind = vec4f(0.0);
        if frame.transmission.y > 0.5 {
            behind = textureSampleLevel(transmission_map, clamp_sampler, uv, frame.transmission.x * blur);
        }
        let around = specular_radiance(bent, blur);
        let light = (behind.rgb + (1.0 - behind.a) * around) * absorption(length(ray));
        f_diffuse = mix(f_diffuse, light * base.rgb, transmission);
    }

    let f_metal = ggx_fresnel(lut, n_dot_v, roughness, base.rgb, 1.0) * f_spec;
    let fresnel_d = ggx_fresnel(lut, n_dot_v, roughness, f0_d, spec_w);
    let f_dielectric = mix(f_diffuse, f_spec, fresnel_d);
    var color = mix(f_dielectric, f_metal, metallic) * occlusion;

    // 10. The key light, shadowed.
    let l = frame.light_dir.xyz;
    let h = normalize(l + v);
    let n_dot_l = clamp(dot(n, l), 0.0, 1.0);
    let v_dot_h = clamp(dot(v, h), 0.0, 1.0);
    let alpha_r = roughness * roughness;
    let lit = select(1.0, shadow(in.world, ng, 1.0), frame.shadow.w > 0.0);
    let light = frame.light_color.rgb * lit;
    if transmission > 0.0 {
        // Light through the surface from behind: the Khronos renderer's
        // BTDF, with the light mirrored to the viewer's side.
        let a_t = alpha_r * clamp(ior * 2.0 - 2.0, 0.0, 1.0);
        let l_mirror = normalize(l - 2.0 * n * dot(l, n));
        let h_t = normalize(l_mirror + v);
        let btdf = base.rgb * d_ggx(clamp(dot(n, h_t), 0.0, 1.0), a_t)
            * v_ggx(clamp(dot(n, l_mirror), 0.0, 1.0), n_dot_v, a_t) * absorption(length(ray));
        var spec = 0.0;
        if n_dot_l > 0.0 {
            spec = d_ggx(clamp(dot(n, h), 0.0, 1.0), alpha_r) * v_ggx(n_dot_l, n_dot_v, alpha_r) * n_dot_l;
        }
        let l_metal = f_schlick(base.rgb, vec3f(1.0), v_dot_h) * spec;
        let l_diffuse = mix(base.rgb / PI * n_dot_l, btdf, transmission);
        let l_dielectric = mix(l_diffuse, vec3f(spec), f_schlick(f0_d * spec_w, vec3f(spec_w), v_dot_h));
        color += light * mix(l_dielectric, l_metal, metallic);
    } else if n_dot_l > 0.0 {
        let spec = d_ggx(clamp(dot(n, h), 0.0, 1.0), alpha_r) * v_ggx(n_dot_l, n_dot_v, alpha_r);
        let l_metal = f_schlick(base.rgb, vec3f(1.0), v_dot_h) * spec;
        let l_dielectric = mix(base.rgb / PI, vec3f(spec), f_schlick(f0_d * spec_w, vec3f(spec_w), v_dot_h));
        color += light * n_dot_l * mix(l_dielectric, l_metal, metallic);
    }

    // 11. Clear coat, on the geometric normal (the model has no clear-coat
    //     normal map).
    let coat = material.extra.z;
    if coat > 0.0 {
        let cr = clamp(material.extra.w, 0.0, 1.0);
        let c_n_dot_v = clamp(dot(ng, v), 0.0, 1.0);
        let c_fresnel = f_schlick(vec3f(f0_ior), vec3f(1.0), c_n_dot_v);
        var c_color = specular_radiance(reflect(-v, ng), cr) * occlusion;
        if screen_occlusion.x < 1.0 || screen_occlusion.y < 1.0 {
            c_color *= specular_occlusion(c_n_dot_v, screen_occlusion, cr);
        }
        let c_n_dot_l = clamp(dot(ng, l), 0.0, 1.0);
        if c_n_dot_l > 0.0 {
            let ca = cr * cr;
            c_color += light * c_n_dot_l * d_ggx(clamp(dot(ng, h), 0.0, 1.0), ca) * v_ggx(c_n_dot_l, c_n_dot_v, ca);
        }
        color = mix(color, c_color, coat * c_fresnel);
    }

    // 12. The fill light: some of the surface's own colour where no light
    //     falls, less where the surface glows by itself already.
    let glow = clamp(max(emissive.r, max(emissive.g, emissive.b)), 0.0, 1.0);
    color += base.rgb * frame.ao.y * (1.0 - glow);
    color += emissive;
    return finish(fogged(tonemap(color), in.world), base.a);
}

// Doom's shading, for materials with fog (`scene::Fog`): the colour, a
// texel times its light decoded with a 2.2 gamma, goes back to the display
// value Doom's source ports compute (they multiply in display values), is
// brightened near the viewer as GZDoom's Doom light mode does, fades into
// the fog by exp(-density * distance), and is shown as that display value.
// Other materials are untouched.
fn fogged(display: vec3f, world: vec3f) -> vec3f {
    if material.fog_near.z < 0.5 {
        return display;
    }
    let d = distance(world, frame.camera.xyz);
    var c = pow(max(display, vec3f(0.0)), vec3f(1.0 / 2.2));
    let near = material.fog_near.y;
    if near > 0.0 && d < near {
        c *= mix(material.fog_near.x, 1.0, d / near);
    }
    let fog = pow(max(material.fog.rgb, vec3f(0.0)), vec3f(1.0 / 2.2));
    c = mix(fog, c, exp(-max(material.fog.w, 0.0) * d));
    return srgb_to_linear(clamp(c, vec3f(0.0), vec3f(1.0)));
}

// How much of the environment's reflection the surface gets, from the
// ambient occlusion pass's two visibilities: Lagarde and de Rousiers'
// estimate from ambient occlusion (tight reflections at grazing angles
// escape it, rough ones don't), and the mirror direction's own, which
// rough reflections, spread wider, trust less.
fn specular_occlusion(n_dot_v: f32, visibility: vec2f, roughness: f32) -> f32 {
    let a = visibility.x;
    let from_ao = clamp(pow(n_dot_v + a, exp2(-16.0 * roughness - 1.0)) - 1.0 + a, 0.0, 1.0);
    return min(from_ao, mix(visibility.y, 1.0, roughness * roughness));
}

// Beer's law: the fraction of light left after `distance` through the
// material's volume, per channel.
fn absorption(distance: f32) -> vec3f {
    let d = material.transmission.z;
    if d <= 0.0 || distance <= 0.0 {
        return vec3f(1.0);
    }
    return pow(max(material.attenuation.rgb, vec3f(1e-6)), vec3f(distance / d));
}

//endregion

/*┌─────────────────╮
| Points          |
└─────────────────╯*/
//region Points

// A dot's diameter in point spacings: enough for neighbours to overlap,
// so a scanned surface looks solid.
const DOT_OVERLAP: f32 = 1.5;

// One triangle per point, around the unit disc that `bary.xy` describes.
// Its radius comes from the draw's point spacing (`misc.z`, world units,
// negative when ambient occlusion leaves the points alone, which `bary.z`
// passes on) and the frame's size settings: in the world, or on screen,
// where 1 is the size at which neighbours meet when the whole scene fills
// the view.
@vertex
fn vs_points(@builtin(vertex_index) vi: u32, @builtin(instance_index) inst: u32) -> VOut {
    let d = instance_draw(inst);
    var o = transform(d, fetch_vertex(d, indexed_vertex(d, vi / 3u)));
    var corners = array<vec2f, 3>(vec2f(0.0, 2.0), vec2f(-1.7320508, -1.0), vec2f(1.7320508, -1.0));
    let c = corners[vi % 3u];
    let spacing = bitcast<f32>(d.misc.z);
    let radius = 0.5 * DOT_OVERLAP * abs(spacing) * frame.points.x;
    if frame.points.y > 0.5 {
        o.clip += vec4f(c * radius * frame.proj.xy, 0.0, 0.0);
    } else {
        let px = clamp(radius * frame.points.z, 0.75, 64.0);
        o.clip += vec4f(c * px * 2.0 * frame.viewport.zw * o.clip.w, 0.0, 0.0);
    }
    o.bary = vec3f(c, spacing);
    return o;
}

struct PointOut {
    @location(0) color: vec4f,
    @builtin(sample_mask) mask: u32,
}

// A round dot with a rim a pixel wide.  The rim's coverage picks how many
// of the pixel's samples the dot takes, so dots stay opaque (no sorting)
// and still get soft edges; with one sample the dot simply ends there.
// Unlit points (scans, whose colours already hold their light) show their
// colour; lit ones are shaded as small spheres unless they have normals.
// Both take ambient occlusion, which gives a plain cloud its depth (the
// pre-pass keeps it off coloured ones).
@fragment
fn fs_points(in: VOut) -> PointOut {
    let r = length(in.bary.xy);
    let w = max(fwidth(r), 1e-4);
    let cover = clamp((1.0 - r) / w + 0.5, 0.0, 1.0);
    let tex = textureSample(t_base, s_base, slot_uv(SLOT_BASE, in.uv0, in.uv1));
    let samples = max(frame.points.w, 1.0);
    var out: PointOut;
    out.mask = (1u << u32(round(cover * samples))) - 1u;
    let base = material.base_color * in.color * tex;
    var color = base.rgb;
    if material.flags.y == 0u {
        var n: vec3f;
        if dot(in.normal, in.normal) > 1e-12 {
            n = normalize(in.normal);
            n *= sign(dot(n, frame.camera.xyz - in.world) + 1e-20);
        } else {
            let along = vec3f(in.bary.xy, sqrt(max(1.0 - r * r, 0.0)));
            let view = mat3x3f(frame.view[0].xyz, frame.view[1].xyz, frame.view[2].xyz);
            n = normalize(transpose(view) * along);
        }
        let key = frame.light_color.rgb * max(dot(n, frame.light_dir.xyz), 0.0) / PI;
        color = (irradiance(n) + key) * base.rgb;
    }
    // Plain clouds are shaded by the surface their dots make, which the
    // ambient occlusion pass works out (1 for coloured clouds).
    let ambient = screen_ao_texel(in.clip.xy, in.world);
    color = color * pow(ambient.x, frame.ao.x) * ambient.w + material.emissive.rgb;
    out.color = finish(tonemap(color), 1.0);
    return out;
}

// Points in the ambient occlusion pre-pass: discs facing the camera.
// Overlapping dots make small steps in depth, which would read as stripes
// and creases all over a scanned surface, so neighbours within a dot's
// width don't occlude; for points that occlusion leaves alone, none do.
@fragment
fn fs_points_prepass(in: VOut) -> @location(0) vec4f {
    if dot(in.bary.xy, in.bary.xy) > 1.0 {
        discard;
    }
    let near = select(65000.0, DOT_OVERLAP * in.bary.z, in.bary.z > 0.0);
    return vec4f(0.0, 0.0, 1.0, near);
}

//endregion

/*┌─────────────────╮
| Overlays        |
└─────────────────╯*/
//region Overlays

@fragment
fn fs_wire(in: VOut) -> @location(0) vec4f {
    let w = fwidth(in.bary);
    let e = smoothstep(vec3f(0.0), w * 1.2, in.bary);
    let a = (1.0 - min(e.x, min(e.y, e.z))) * frame.misc.w;
    return vec4f(to_target(vec3f(1.0, 0.55, 0.12)) * a, a);
}

struct LineOut {
    @builtin(position) clip: vec4f,
    @location(0) color: vec4f,
}

// Normals and tangents as short lines: two vertices per mesh vertex, the
// second pushed out along the vector.
fn vector_line(i: u32, inst: u32, tangents: bool) -> LineOut {
    let d = instance_draw(inst);
    let vtx = fetch_vertex(d, i / 2u);
    let world = (d.model * vec4f(vtx.pos, 1.0)).xyz;
    var dir: vec3f;
    var o: LineOut;
    if tangents {
        dir = (d.model * vec4f(vtx.tangent.xyz, 0.0)).xyz;
        o.color = select(vec4f(1.0, 0.25, 0.2, 1.0), vec4f(1.0, 0.8, 0.1, 1.0), vtx.tangent.w < 0.0);
    } else {
        dir = (d.normal * vec4f(vtx.normal, 0.0)).xyz;
        o.color = vec4f(0.25, 0.55, 1.0, 1.0);
    }
    let len = length(dir);
    dir = select(vec3f(0.0), dir / len, len > 1e-12);
    let p = world + dir * frame.misc.y * f32(i % 2u);
    o.clip = frame.view_proj * vec4f(p, 1.0);
    return o;
}

@vertex
fn vs_normals(@builtin(vertex_index) i: u32, @builtin(instance_index) inst: u32) -> LineOut {
    return vector_line(i, inst, false);
}

@vertex
fn vs_tangents(@builtin(vertex_index) i: u32, @builtin(instance_index) inst: u32) -> LineOut {
    return vector_line(i, inst, true);
}

// Lines the CPU builds: bounds, skeleton, markers.
@vertex
fn vs_lines(@location(0) pos: vec3f, @location(1) color: vec4f) -> LineOut {
    var o: LineOut;
    o.clip = frame.view_proj * vec4f(pos, 1.0);
    o.color = color;
    return o;
}

@fragment
fn fs_line(in: LineOut) -> @location(0) vec4f {
    return vec4f(to_target(in.color.rgb) * in.color.a, in.color.a);
}

//endregion

/*┌─────────────────╮
| Backdrop        |
└─────────────────╯*/
//region Backdrop

struct ScreenOut {
    @builtin(position) clip: vec4f,
    @location(0) ndc: vec2f,
}

@vertex
fn vs_fullscreen(@builtin(vertex_index) i: u32) -> ScreenOut {
    var o: ScreenOut;
    o.clip = fullscreen(i);
    o.ndc = o.clip.xy;
    return o;
}

// The environment behind the scene, optionally blurred.
@fragment
fn fs_background(in: ScreenOut) -> @location(0) vec4f {
    let far = frame.inv_view_proj * vec4f(in.ndc, 1.0, 1.0);
    let near = frame.inv_view_proj * vec4f(in.ndc, 0.0, 1.0);
    let dir = normalize(far.xyz / far.w - near.xyz / near.w);
    var c: vec3f;
    if frame.tone.w <= 0.0 {
        c = textureSampleLevel(sharp_env, env_sampler, env_dir(dir), 0.0).rgb;
    } else {
        c = textureSampleLevel(radiance, env_sampler, env_dir(dir), frame.tone.w).rgb;
    }
    return vec4f(to_target(tonemap(c * frame.env.z)), 1.0);
}

struct GroundOut {
    @builtin(position) clip: vec4f,
    @location(0) world: vec3f,
}

fn ground_vertex(i: u32, radius: f32) -> GroundOut {
    var corners = array<vec2f, 6>(
        vec2f(-1.0, -1.0), vec2f(1.0, 1.0), vec2f(1.0, -1.0),
        vec2f(-1.0, -1.0), vec2f(-1.0, 1.0), vec2f(1.0, 1.0)
    );
    let c = corners[i];
    var o: GroundOut;
    o.world = vec3f(frame.ground_center.x + c.x * radius, frame.ground.x, frame.ground_center.y + c.y * radius);
    o.clip = frame.view_proj * vec4f(o.world, 1.0);
    return o;
}

@vertex
fn vs_ground(@builtin(vertex_index) i: u32) -> GroundOut {
    return ground_vertex(i, frame.ground.z);
}

const POISSON = array<vec2f, 12>(
    vec2f(-0.326, -0.406), vec2f(-0.840, -0.074), vec2f(-0.696, 0.457), vec2f(-0.203, 0.621),
    vec2f(0.962, -0.195), vec2f(0.473, -0.480), vec2f(0.519, 0.767), vec2f(0.185, -0.893),
    vec2f(0.507, 0.064), vec2f(0.896, 0.412), vec2f(-0.322, -0.933), vec2f(-0.792, -0.598)
);

// A shadow catcher: black with the strength of the contact shadow and the
// key light's shadow as alpha, fading out towards its edge.
@fragment
fn fs_ground(in: GroundOut) -> @location(0) vec4f {
    let r = length(in.world.xz - frame.ground_center.xy) / max(frame.ground.z, 1e-6);
    let fade = 1.0 - smoothstep(0.55, 1.0, r);
    let pc = frame.contact_view_proj * vec4f(in.world, 1.0);
    let uv = pc.xy * vec2f(0.5, -0.5) + 0.5;
    var occ = textureSampleLevel(contact_map, clamp_sampler, uv, 0.0).r;
    for (var i = 0; i < 12; i++) {
        occ += textureSampleLevel(contact_map, clamp_sampler, uv + POISSON[i] * frame.shadow.z, 0.0).r;
    }
    occ = occ / 13.0 * frame.ground.w;
    let lit = shadow(in.world, vec3f(0.0, 1.0, 0.0), 2.5);
    let a = clamp(max(occ, (1.0 - lit) * 0.55), 0.0, 1.0) * fade;
    return vec4f(0.0, 0.0, 0.0, a);
}

@vertex
fn vs_grid(@builtin(vertex_index) i: u32) -> GroundOut {
    return ground_vertex(i, frame.ground_center.w);
}

fn grid_lines(p: vec2f, spacing: f32) -> f32 {
    let q = p / spacing;
    let w = fwidth(q);
    let g = abs(fract(q - 0.5) - 0.5) / max(w, vec2f(1e-6));
    return 1.0 - clamp(min(g.x, g.y), 0.0, 1.0);
}

// A metric grid: lines every `spacing` and fainter tenths, the X axis red
// and the Z axis blue, fading with distance.
@fragment
fn fs_grid(in: GroundOut) -> @location(0) vec4f {
    let spacing = frame.ground_center.z;
    let p = in.world.xz;
    let major = grid_lines(p, spacing);
    // Tenths fade out before they are packed tighter than a few pixels.
    let cell_px = spacing * 0.1 / max(length(fwidth(p)), 1e-6);
    let minor = grid_lines(p, spacing * 0.1) * smoothstep(3.0, 10.0, cell_px);
    let w = fwidth(p);
    let x_axis = 1.0 - clamp(abs(p.y) / max(w.y, 1e-6), 0.0, 1.0);
    let z_axis = 1.0 - clamp(abs(p.x) / max(w.x, 1e-6), 0.0, 1.0);
    let r = length(p - frame.ground_center.xy) / max(frame.ground_center.w, 1e-6);
    let fade = 1.0 - smoothstep(0.5, 1.0, r);
    var color = vec3f(0.5);
    var a = max(major * 0.45, minor * 0.18);
    if x_axis > 0.0 {
        color = mix(color, vec3f(0.85, 0.2, 0.2), x_axis);
        a = max(a, x_axis * 0.8);
    }
    if z_axis > 0.0 {
        color = mix(color, vec3f(0.2, 0.35, 0.9), z_axis);
        a = max(a, z_axis * 0.8);
    }
    a *= fade;
    return vec4f(to_target(color) * a, a);
}

//endregion
