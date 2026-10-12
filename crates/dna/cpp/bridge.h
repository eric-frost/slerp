// The C++ half of slerp-dna's bridge to openriglogic: a few free functions
// over two opaque types, so the Rust side never sees openriglogic's class
// hierarchy.  Everything that reads the file or allocates through
// openriglogic is declared fallible on the Rust side, where cxx turns the
// C++ exceptions thrown here into errors.

#pragma once

#include "rust/cxx.h"

#include <cstdint>
#include <memory>

namespace slerp_dna {

// A DNA file read with openriglogic's BinaryStreamReader.  cxx needs the
// complete type, so openriglogic's own objects hide behind `Impl`.
class Dna {
public:
    struct Impl;
    explicit Dna(Impl* impl_);
    ~Dna();
    Dna(const Dna&) = delete;
    Dna& operator=(const Dna&) = delete;
    Impl* impl;
};

// RigLogic built from a Dna, with the one rig instance the viewer drives.
class Rig {
public:
    struct Impl;
    explicit Rig(Impl* impl_);
    ~Rig();
    Rig(const Rig&) = delete;
    Rig& operator=(const Rig&) = delete;
    Impl* impl;
};

// Reads the DNA file at `path` (UTF-8 bytes) up to the data layers in
// `layer` (dna::DataLayer's bits).  `lods` lists the levels of detail to
// keep, all of them when empty.  Allocations past `budget` bytes fail.
std::unique_ptr<Dna> read_dna(rust::Slice<const std::uint8_t> path, std::uint32_t layer,
                              rust::Slice<const std::uint16_t> lods, std::uint64_t budget);
// Bytes the reader holds at most while reading, to size budgets.
std::uint64_t dna_peak_bytes(const Dna& dna);

// Descriptor.
rust::String dna_name(const Dna& dna);
std::uint16_t dna_lod_count(const Dna& dna);
// Translation unit, rotation unit, coordinate system x, y and z, rotation
// sequence and face winding, as openriglogic's enum values.
rust::Vec<std::uint32_t> dna_conventions(const Dna& dna);

// Definition.
std::uint16_t joint_count(const Dna& dna);
rust::String joint_name(const Dna& dna, std::uint16_t index);
std::uint16_t joint_parent(const Dna& dna, std::uint16_t index);
// Six floats per joint: neutral translation, then neutral rotation.
rust::Vec<float> neutral_joints(const Dna& dna);
rust::Vec<std::uint16_t> joints_for_lod(const Dna& dna, std::uint16_t lod);
std::uint16_t raw_control_count(const Dna& dna);
rust::String raw_control_name(const Dna& dna, std::uint16_t index);
std::uint16_t channel_count(const Dna& dna);
rust::String channel_name(const Dna& dna, std::uint16_t index);
std::uint16_t animated_map_count(const Dna& dna);
std::uint16_t mesh_count(const Dna& dna);
rust::String mesh_name(const Dna& dna, std::uint16_t index);
rust::Vec<std::uint16_t> meshes_for_lod(const Dna& dna, std::uint16_t lod);

// Geometry of one mesh, flattened: xyz per position and normal, uv per
// texture coordinate, (position, uv, normal) indices per vertex layout,
// and each face as its corner count followed by its layout indices.
rust::Vec<float> mesh_positions(const Dna& dna, std::uint16_t mesh);
rust::Vec<float> mesh_normals(const Dna& dna, std::uint16_t mesh);
rust::Vec<float> mesh_uvs(const Dna& dna, std::uint16_t mesh);
rust::Vec<std::uint32_t> mesh_layouts(const Dna& dna, std::uint16_t mesh);
rust::Vec<std::uint32_t> mesh_faces(const Dna& dna, std::uint16_t mesh);
// Skin weights per position: the influence counts, then all joints and all
// weights in order.
void mesh_skin(const Dna& dna, std::uint16_t mesh, rust::Vec<std::uint32_t>& counts,
               rust::Vec<std::uint16_t>& joints, rust::Vec<float>& weights);
std::uint16_t target_count(const Dna& dna, std::uint16_t mesh);
std::uint16_t target_channel(const Dna& dna, std::uint16_t mesh, std::uint16_t target);
// One blend shape target: the positions it moves and xyz per position.
void mesh_target(const Dna& dna, std::uint16_t mesh, std::uint16_t target, rust::Vec<std::uint32_t>& vertices,
                 rust::Vec<float>& deltas);

// RigLogic.  `budget` caps its own allocations.
std::unique_ptr<Rig> make_rig(const Dna& dna, std::uint64_t budget);
std::uint16_t rig_lod_count(const Rig& rig);
void rig_set_lod(Rig& rig, std::uint16_t lod);
std::uint16_t rig_control_count(const Rig& rig);
// Sets every raw control (missing ones to 0) and evaluates the rig.
void rig_evaluate(Rig& rig, rust::Slice<const float> controls);
// The same, but only as far as the joints: no blend shapes or animated maps.
void rig_evaluate_joints(Rig& rig, rust::Slice<const float> controls);
// The last evaluation: nine floats per joint added to the neutral values,
// one weight per blend shape channel, one value per animated map.
rust::Slice<const float> rig_joint_outputs(const Rig& rig);
rust::Slice<const float> rig_channel_outputs(const Rig& rig);
rust::Slice<const float> rig_map_outputs(const Rig& rig);

}  // namespace slerp_dna
