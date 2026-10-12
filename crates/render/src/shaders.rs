//! The WGSL sources, assembled from shared pieces.  wgpu has no include
//! mechanism, so modules are concatenated at compile time.

pub(crate) const SCENE: &str = concat!(
    include_str!("shaders/color.wgsl"),
    include_str!("shaders/common.wgsl"),
    include_str!("shaders/lighting.wgsl"),
    include_str!("shaders/scene.wgsl"),
);

pub(crate) const IBL: &str = concat!(include_str!("shaders/color.wgsl"), include_str!("shaders/ibl.wgsl"));

pub(crate) const TEXVIEW: &str = concat!(include_str!("shaders/color.wgsl"), include_str!("shaders/texview.wgsl"));

pub(crate) const MIPGEN: &str = concat!(include_str!("shaders/color.wgsl"), include_str!("shaders/mipgen.wgsl"));

pub(crate) const AO: &str = concat!(include_str!("shaders/color.wgsl"), include_str!("shaders/ao.wgsl"));

pub(crate) const SPLAT_PROJECT: &str = include_str!("shaders/splat_project.wgsl");

pub(crate) const SPLAT_SORT: &str = include_str!("shaders/splat_sort.wgsl");

pub(crate) const SPLAT_DRAW: &str = include_str!("shaders/splat_draw.wgsl");

pub(crate) const SPLAT_COMPOSITE: &str = concat!(
    include_str!("shaders/color.wgsl"),
    include_str!("shaders/common.wgsl"),
    include_str!("shaders/splat_composite.wgsl"),
);

pub(crate) fn module(device: &wgpu::Device, label: &str, source: &str) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    })
}

#[cfg(test)]
mod tests {
    fn validate(name: &str, source: &str) {
        let module = match naga::front::wgsl::parse_str(source) {
            Ok(m) => m,
            Err(e) => panic!("{name}: {}", e.emit_to_string(source)),
        };
        let mut v = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all());
        let info = match v.validate(&module) {
            Ok(info) => info,
            Err(e) => panic!("{name}: {}", e.emit_to_string(source)),
        };
        // What wgpu does on macOS and Windows: Metal Shading Language and
        // HLSL, so a construct those backends can't express shows up here
        // rather than on someone's Mac.
        let metal = naga::back::msl::Options { lang_version: (2, 4), ..Default::default() };
        let msl = naga::back::msl::write_string(&module, &info, &metal, &Default::default());
        assert!(msl.is_ok(), "{name} to Metal: {:?}", msl.err());
        let mut hlsl = String::new();
        let (options, pipeline) = (Default::default(), Default::default());
        let written = naga::back::hlsl::Writer::new(&mut hlsl, &options, &pipeline).write(&module, &info, None);
        assert!(written.is_ok(), "{name} to HLSL: {:?}", written.err());
    }

    #[test]
    fn shaders_validate() {
        validate("scene", super::SCENE);
        validate("ibl", super::IBL);
        validate("texview", super::TEXVIEW);
        validate("mipgen", super::MIPGEN);
        validate("ambient occlusion", super::AO);
        validate("splat project", super::SPLAT_PROJECT);
        validate("splat sort", super::SPLAT_SORT);
        validate("splat draw", super::SPLAT_DRAW);
        validate("splat composite", super::SPLAT_COMPOSITE);
    }
}
