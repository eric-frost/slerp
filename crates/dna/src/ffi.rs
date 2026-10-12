//! The Rust half of the bridge to openriglogic (`cpp/bridge.h`).  Only
//! `extract` and `rig` call these.

#[cxx::bridge(namespace = "slerp_dna")]
pub(crate) mod bridge {
    unsafe extern "C++" {
        include!("slerp-dna/cpp/bridge.h");

        type Dna;
        type Rig;

        fn read_dna(path: &[u8], layer: u32, lods: &[u16], budget: u64) -> Result<UniquePtr<Dna>>;
        fn dna_peak_bytes(dna: &Dna) -> u64;

        fn dna_name(dna: &Dna) -> String;
        fn dna_lod_count(dna: &Dna) -> u16;
        fn dna_conventions(dna: &Dna) -> Vec<u32>;

        fn joint_count(dna: &Dna) -> u16;
        fn joint_name(dna: &Dna, index: u16) -> String;
        fn joint_parent(dna: &Dna, index: u16) -> u16;
        fn neutral_joints(dna: &Dna) -> Vec<f32>;
        fn joints_for_lod(dna: &Dna, lod: u16) -> Vec<u16>;
        fn raw_control_count(dna: &Dna) -> u16;
        fn raw_control_name(dna: &Dna, index: u16) -> String;
        fn channel_count(dna: &Dna) -> u16;
        fn channel_name(dna: &Dna, index: u16) -> String;
        fn animated_map_count(dna: &Dna) -> u16;
        fn mesh_count(dna: &Dna) -> u16;
        fn mesh_name(dna: &Dna, index: u16) -> String;
        fn meshes_for_lod(dna: &Dna, lod: u16) -> Vec<u16>;

        fn mesh_positions(dna: &Dna, mesh: u16) -> Vec<f32>;
        fn mesh_normals(dna: &Dna, mesh: u16) -> Vec<f32>;
        fn mesh_uvs(dna: &Dna, mesh: u16) -> Vec<f32>;
        fn mesh_layouts(dna: &Dna, mesh: u16) -> Vec<u32>;
        fn mesh_faces(dna: &Dna, mesh: u16) -> Vec<u32>;
        fn mesh_skin(dna: &Dna, mesh: u16, counts: &mut Vec<u32>, joints: &mut Vec<u16>, weights: &mut Vec<f32>);
        fn target_count(dna: &Dna, mesh: u16) -> u16;
        fn target_channel(dna: &Dna, mesh: u16, target: u16) -> u16;
        fn mesh_target(dna: &Dna, mesh: u16, target: u16, vertices: &mut Vec<u32>, deltas: &mut Vec<f32>);

        fn make_rig(dna: &Dna, budget: u64) -> Result<UniquePtr<Rig>>;
        fn rig_lod_count(rig: &Rig) -> u16;
        fn rig_set_lod(rig: Pin<&mut Rig>, lod: u16);
        fn rig_control_count(rig: &Rig) -> u16;
        fn rig_evaluate(rig: Pin<&mut Rig>, controls: &[f32]) -> Result<()>;
        fn rig_evaluate_joints(rig: Pin<&mut Rig>, controls: &[f32]) -> Result<()>;
        fn rig_joint_outputs(rig: &Rig) -> &[f32];
        fn rig_channel_outputs(rig: &Rig) -> &[f32];
        fn rig_map_outputs(rig: &Rig) -> &[f32];
    }
}

// SAFETY: a `Dna` or `Rig` owns all its memory (its own allocator, reader,
// RigLogic and instance) and shares nothing with other objects, and the
// bridge only reads a `Dna` through shared references and changes a `Rig`
// through exclusive ones, so moving either to another thread is safe.
unsafe impl Send for bridge::Dna {}
unsafe impl Send for bridge::Rig {}
// SAFETY: every function taking `&Dna` only reads openriglogic's reader,
// whose getters are const and touch no shared state; status codes are
// thread-local.
unsafe impl Sync for bridge::Dna {}
