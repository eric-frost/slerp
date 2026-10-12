// The C++ half of slerp-dna's bridge to openriglogic.  See bridge.h.

#include "slerp-dna/cpp/bridge.h"

#include <dna/BinaryStreamReader.h>
#include <dna/Reader.h>
#include <pma/MemoryResource.h>
#include <pma/resources/AlignedMemoryResource.h>
#include <riglogic/RigLogic.h>
#include <status/Provider.h>
#include <status/Status.h>
#include <trio/streams/FileStream.h>

#include <algorithm>
#include <cstring>
#include <new>
#include <stdexcept>
#include <string>
#include <vector>

namespace slerp_dna {

namespace {

// A memory resource that refuses to hand out more than `limit` bytes in
// all.  openriglogic sizes its containers from counts in the file, bounded
// only by the file's length, so a damaged file could otherwise ask for
// many times its size.  Refusing throws std::bad_alloc, which unwinds out
// of the reader (no openriglogic function that allocates is noexcept) and
// reaches Rust as an error.
class Budget : public pma::MemoryResource {
public:
    explicit Budget(std::uint64_t limit_) : limit{limit_}, used{0}, peak{0} {
    }

    void* allocate(std::size_t size, std::size_t alignment) override {
        if (size > limit || used > limit - size) {
            throw std::bad_alloc();
        }
        void* ptr = inner.allocate(size, alignment);
        if (ptr == nullptr) {
            throw std::bad_alloc();
        }
        used += size;
        peak = std::max(peak, used);
        return ptr;
    }

    void deallocate(void* ptr, std::size_t size, std::size_t alignment) override {
        inner.deallocate(ptr, size, alignment);
        used -= std::min<std::uint64_t>(used, size);
    }

    std::uint64_t peakBytes() const {
        return peak;
    }

private:
    pma::AlignedMemoryResource inner;
    std::uint64_t limit;
    std::uint64_t used;
    std::uint64_t peak;
};

// Throws the status openriglogic recorded on this thread, if it isn't OK.
void check(const char* what) {
    if (!sc::Status::isOk()) {
        const auto status = sc::Status::get();
        throw std::runtime_error(std::string(what) + ": " + (status.message ? status.message : "unknown error"));
    }
}

rust::String str(const char* data, std::size_t size) {
    // Names in a damaged file needn't be UTF-8; lossy keeps them readable.
    return rust::String::lossy(data, size);
}

template<typename T>
rust::Vec<T> vec(const T* data, std::size_t size) {
    rust::Vec<T> out;
    out.reserve(size);
    for (std::size_t i = 0; i < size; ++i) {
        out.push_back(data[i]);
    }
    return out;
}

}  // namespace

struct Dna::Impl {
    explicit Impl(std::uint64_t budget) : memory{budget}, stream{nullptr}, reader{nullptr} {
    }

    ~Impl() {
        if (reader != nullptr) {
            dna::BinaryStreamReader::destroy(reader);
        }
        if (stream != nullptr) {
            trio::FileStream::destroy(stream);
        }
    }

    Impl(const Impl&) = delete;
    Impl& operator=(const Impl&) = delete;

    // Declared first, so it outlives everything allocated from it.
    Budget memory;
    trio::FileStream* stream;
    dna::BinaryStreamReader* reader;
};

Dna::Dna(Impl* impl_) : impl{impl_} {
}

Dna::~Dna() {
    delete impl;
}

struct Rig::Impl {
    explicit Impl(std::uint64_t budget) : memory{budget}, logic{nullptr}, instance{nullptr} {
    }

    ~Impl() {
        if (instance != nullptr) {
            rl4::RigInstance::destroy(instance);
        }
        if (logic != nullptr) {
            rl4::RigLogic::destroy(logic);
        }
    }

    Impl(const Impl&) = delete;
    Impl& operator=(const Impl&) = delete;

    Budget memory;
    rl4::RigLogic* logic;
    rl4::RigInstance* instance;
};

Rig::Rig(Impl* impl_) : impl{impl_} {
}

Rig::~Rig() {
    delete impl;
}

/*┌─────────────────╮
  | Reading         |
  └─────────────────╯*/

std::unique_ptr<Dna> read_dna(rust::Slice<const std::uint8_t> path, std::uint32_t layer,
                              rust::Slice<const std::uint16_t> lods, std::uint64_t budget) {
    sc::StatusProvider::reset();
    const std::string p(reinterpret_cast<const char*>(path.data()), path.size());
    auto out = std::unique_ptr<Dna>(new Dna(new Dna::Impl(budget)));
    auto* d = out->impl;
    d->stream = trio::FileStream::create(p.c_str(), trio::AccessMode::Read, trio::OpenMode::Binary, &d->memory);
    check("opening the file");
    std::vector<std::uint16_t> wanted(lods.begin(), lods.end());
    dna::Configuration config;
    config.layer = static_cast<dna::DataLayer>(layer);
    config.unknownLayerPolicy = dna::UnknownLayerPolicy::Ignore;
    // Slerp's axes, which are glTF's and those of MetaHuman Creator's own
    // exports: +X the character's left, +Y up, +Z its front, Euler angles
    // about X first, faces counter-clockwise.  openriglogic converts files
    // written otherwise (geometry, joints and the rig alike) and leaves
    // these as they are.
    config.coordinateSystemTransformPolicy = dna::CoordinateSystemTransformPolicy::Transform;
    config.coordinateSystem = {dna::Direction::left, dna::Direction::up, dna::Direction::front};
    config.rotationSequence = dna::RotationSequence::xyz;
    config.rotationSign = {dna::RotationDirection::positive, dna::RotationDirection::positive,
                           dna::RotationDirection::positive};
    config.faceWindingOrder = dna::FaceWindingOrder::ccw;
    if (!wanted.empty()) {
        config.lods = dna::ConstArrayView<std::uint16_t>{wanted.data(), wanted.size()};
    }
    d->reader = dna::BinaryStreamReader::create(d->stream, config, &d->memory);
    check("starting the reader");
    d->reader->read();
    check("reading");
    // The stream is only needed while reading.
    trio::FileStream::destroy(d->stream);
    d->stream = nullptr;
    return out;
}

std::uint64_t dna_peak_bytes(const Dna& dna) {
    return dna.impl->memory.peakBytes();
}

/*┌─────────────────╮
  | Descriptor      |
  └─────────────────╯*/

rust::String dna_name(const Dna& dna) {
    const auto name = dna.impl->reader->getName();
    return str(name.data(), name.size());
}

std::uint16_t dna_lod_count(const Dna& dna) {
    return dna.impl->reader->getLODCount();
}

rust::Vec<std::uint32_t> dna_conventions(const Dna& dna) {
    const auto* r = dna.impl->reader;
    const auto cs = r->getCoordinateSystem();
    rust::Vec<std::uint32_t> out;
    out.push_back(static_cast<std::uint32_t>(r->getTranslationUnit()));
    out.push_back(static_cast<std::uint32_t>(r->getRotationUnit()));
    out.push_back(static_cast<std::uint32_t>(cs.x));
    out.push_back(static_cast<std::uint32_t>(cs.y));
    out.push_back(static_cast<std::uint32_t>(cs.z));
    out.push_back(static_cast<std::uint32_t>(r->getRotationSequence()));
    out.push_back(static_cast<std::uint32_t>(r->getFaceWindingOrder()));
    return out;
}

/*┌─────────────────╮
  | Definition      |
  └─────────────────╯*/

std::uint16_t joint_count(const Dna& dna) {
    return dna.impl->reader->getJointCount();
}

rust::String joint_name(const Dna& dna, std::uint16_t index) {
    const auto name = dna.impl->reader->getJointName(index);
    return str(name.data(), name.size());
}

std::uint16_t joint_parent(const Dna& dna, std::uint16_t index) {
    return dna.impl->reader->getJointParentIndex(index);
}

rust::Vec<float> neutral_joints(const Dna& dna) {
    const auto* r = dna.impl->reader;
    const auto tx = r->getNeutralJointTranslationXs();
    const auto ty = r->getNeutralJointTranslationYs();
    const auto tz = r->getNeutralJointTranslationZs();
    const auto rx = r->getNeutralJointRotationXs();
    const auto ry = r->getNeutralJointRotationYs();
    const auto rz = r->getNeutralJointRotationZs();
    std::size_t n = r->getJointCount();
    for (const auto* a : {&tx, &ty, &tz, &rx, &ry, &rz}) {
        n = std::min(n, a->size());
    }
    rust::Vec<float> out;
    out.reserve(n * 6);
    for (std::size_t i = 0; i < n; ++i) {
        for (const auto* a : {&tx, &ty, &tz, &rx, &ry, &rz}) {
            out.push_back((*a)[i]);
        }
    }
    return out;
}

rust::Vec<std::uint16_t> joints_for_lod(const Dna& dna, std::uint16_t lod) {
    if (lod >= dna.impl->reader->getJointIndexListCount() && lod >= dna.impl->reader->getLODCount()) {
        return {};
    }
    const auto v = dna.impl->reader->getJointIndicesForLOD(lod);
    return vec(v.data(), v.size());
}

std::uint16_t raw_control_count(const Dna& dna) {
    return dna.impl->reader->getRawControlCount();
}

rust::String raw_control_name(const Dna& dna, std::uint16_t index) {
    const auto name = dna.impl->reader->getRawControlName(index);
    return str(name.data(), name.size());
}

std::uint16_t channel_count(const Dna& dna) {
    return dna.impl->reader->getBlendShapeChannelCount();
}

rust::String channel_name(const Dna& dna, std::uint16_t index) {
    const auto name = dna.impl->reader->getBlendShapeChannelName(index);
    return str(name.data(), name.size());
}

std::uint16_t animated_map_count(const Dna& dna) {
    return dna.impl->reader->getAnimatedMapCount();
}

std::uint16_t mesh_count(const Dna& dna) {
    return dna.impl->reader->getMeshCount();
}

rust::String mesh_name(const Dna& dna, std::uint16_t index) {
    const auto name = dna.impl->reader->getMeshName(index);
    return str(name.data(), name.size());
}

rust::Vec<std::uint16_t> meshes_for_lod(const Dna& dna, std::uint16_t lod) {
    if (lod >= dna.impl->reader->getLODCount()) {
        return {};
    }
    const auto v = dna.impl->reader->getMeshIndicesForLOD(lod);
    return vec(v.data(), v.size());
}

/*┌─────────────────╮
  | Geometry        |
  └─────────────────╯*/

namespace {

template<typename A>
rust::Vec<float> interleave(std::initializer_list<const A*> arrays) {
    std::size_t n = SIZE_MAX;
    for (const auto* a : arrays) {
        n = std::min(n, a->size());
    }
    if (n == SIZE_MAX) {
        n = 0;
    }
    rust::Vec<float> out;
    out.reserve(n * arrays.size());
    for (std::size_t i = 0; i < n; ++i) {
        for (const auto* a : arrays) {
            out.push_back((*a)[i]);
        }
    }
    return out;
}

}  // namespace

rust::Vec<float> mesh_positions(const Dna& dna, std::uint16_t mesh) {
    const auto* r = dna.impl->reader;
    const auto x = r->getVertexPositionXs(mesh);
    const auto y = r->getVertexPositionYs(mesh);
    const auto z = r->getVertexPositionZs(mesh);
    return interleave({&x, &y, &z});
}

rust::Vec<float> mesh_normals(const Dna& dna, std::uint16_t mesh) {
    const auto* r = dna.impl->reader;
    const auto x = r->getVertexNormalXs(mesh);
    const auto y = r->getVertexNormalYs(mesh);
    const auto z = r->getVertexNormalZs(mesh);
    return interleave({&x, &y, &z});
}

rust::Vec<float> mesh_uvs(const Dna& dna, std::uint16_t mesh) {
    const auto* r = dna.impl->reader;
    const auto u = r->getVertexTextureCoordinateUs(mesh);
    const auto v = r->getVertexTextureCoordinateVs(mesh);
    return interleave({&u, &v});
}

rust::Vec<std::uint32_t> mesh_layouts(const Dna& dna, std::uint16_t mesh) {
    const auto* r = dna.impl->reader;
    const auto p = r->getVertexLayoutPositionIndices(mesh);
    const auto t = r->getVertexLayoutTextureCoordinateIndices(mesh);
    const auto n = r->getVertexLayoutNormalIndices(mesh);
    const std::size_t count = std::min({p.size(), t.size(), n.size()});
    rust::Vec<std::uint32_t> out;
    out.reserve(count * 3);
    for (std::size_t i = 0; i < count; ++i) {
        out.push_back(p[i]);
        out.push_back(t[i]);
        out.push_back(n[i]);
    }
    return out;
}

rust::Vec<std::uint32_t> mesh_faces(const Dna& dna, std::uint16_t mesh) {
    const auto* r = dna.impl->reader;
    const std::uint32_t faces = r->getFaceCount(mesh);
    rust::Vec<std::uint32_t> out;
    out.reserve(static_cast<std::size_t>(faces) * 5);
    for (std::uint32_t f = 0; f < faces; ++f) {
        const auto corners = r->getFaceVertexLayoutIndices(mesh, f);
        out.push_back(static_cast<std::uint32_t>(corners.size()));
        for (const auto c : corners) {
            out.push_back(c);
        }
    }
    return out;
}

void mesh_skin(const Dna& dna, std::uint16_t mesh, rust::Vec<std::uint32_t>& counts,
               rust::Vec<std::uint16_t>& joints, rust::Vec<float>& weights) {
    const auto* r = dna.impl->reader;
    const std::uint32_t n = r->getSkinWeightsCount(mesh);
    counts.reserve(n);
    for (std::uint32_t v = 0; v < n; ++v) {
        const auto j = r->getSkinWeightsJointIndices(mesh, v);
        const auto w = r->getSkinWeightsValues(mesh, v);
        const std::size_t k = std::min(j.size(), w.size());
        counts.push_back(static_cast<std::uint32_t>(k));
        for (std::size_t i = 0; i < k; ++i) {
            joints.push_back(j[i]);
            weights.push_back(w[i]);
        }
    }
}

std::uint16_t target_count(const Dna& dna, std::uint16_t mesh) {
    return dna.impl->reader->getBlendShapeTargetCount(mesh);
}

std::uint16_t target_channel(const Dna& dna, std::uint16_t mesh, std::uint16_t target) {
    return dna.impl->reader->getBlendShapeChannelIndex(mesh, target);
}

void mesh_target(const Dna& dna, std::uint16_t mesh, std::uint16_t target, rust::Vec<std::uint32_t>& vertices,
                 rust::Vec<float>& deltas) {
    const auto* r = dna.impl->reader;
    const auto ids = r->getBlendShapeTargetVertexIndices(mesh, target);
    const auto x = r->getBlendShapeTargetDeltaXs(mesh, target);
    const auto y = r->getBlendShapeTargetDeltaYs(mesh, target);
    const auto z = r->getBlendShapeTargetDeltaZs(mesh, target);
    const std::size_t n = std::min({ids.size(), x.size(), y.size(), z.size()});
    vertices.reserve(n);
    deltas.reserve(n * 3);
    for (std::size_t i = 0; i < n; ++i) {
        vertices.push_back(ids[i]);
        deltas.push_back(x[i]);
        deltas.push_back(y[i]);
        deltas.push_back(z[i]);
    }
}

/*┌─────────────────╮
  | RigLogic        |
  └─────────────────╯*/

std::unique_ptr<Rig> make_rig(const Dna& dna, std::uint64_t budget) {
    sc::StatusProvider::reset();
    auto out = std::unique_ptr<Rig>(new Rig(new Rig::Impl(budget)));
    auto* g = out->impl;
    rl4::Configuration config;
    // Full floats, as for offline evaluation: half floats need F16C, and the
    // vector paths are left to RigLogic's own choice of what was built in.
    config.floatingPointType = rl4::FloatingPointType::Float;
    config.calculationType = rl4::CalculationType::Scalar;
    g->logic = rl4::RigLogic::create(dna.impl->reader, config, &g->memory);
    check("building the rig");
    if (g->logic == nullptr) {
        throw std::runtime_error("RigLogic refused the file");
    }
    g->instance = rl4::RigInstance::create(g->logic, &g->memory);
    check("starting the rig");
    if (g->instance == nullptr) {
        throw std::runtime_error("RigLogic couldn't start an instance");
    }
    return out;
}

std::uint16_t rig_lod_count(const Rig& rig) {
    return rig.impl->logic->getLODCount();
}

void rig_set_lod(Rig& rig, std::uint16_t lod) {
    const std::uint16_t count = rig.impl->logic->getLODCount();
    if (count > 0) {
        rig.impl->instance->setLOD(std::min<std::uint16_t>(lod, static_cast<std::uint16_t>(count - 1)));
    }
}

std::uint16_t rig_control_count(const Rig& rig) {
    return rig.impl->instance->getRawControlCount();
}

void rig_evaluate(Rig& rig, rust::Slice<const float> controls) {
    sc::StatusProvider::reset();
    const std::uint16_t n = rig.impl->instance->getRawControlCount();
    for (std::uint16_t i = 0; i < n; ++i) {
        rig.impl->instance->setRawControl(i, i < controls.size() ? controls[i] : 0.0f);
    }
    rig.impl->logic->calculate(rig.impl->instance);
    check("evaluating the rig");
}

void rig_evaluate_joints(Rig& rig, rust::Slice<const float> controls) {
    sc::StatusProvider::reset();
    const std::uint16_t n = rig.impl->instance->getRawControlCount();
    for (std::uint16_t i = 0; i < n; ++i) {
        rig.impl->instance->setRawControl(i, i < controls.size() ? controls[i] : 0.0f);
    }
    // RigLogic::calculate's own order, stopping after the joints.
    const auto* logic = rig.impl->logic;
    logic->calculateMLControls(rig.impl->instance);
    logic->calculateRBFControls(rig.impl->instance);
    logic->calculatePSDControls(rig.impl->instance);
    logic->calculateJoints(rig.impl->instance);
    check("evaluating the rig");
}

rust::Slice<const float> rig_joint_outputs(const Rig& rig) {
    const auto v = rig.impl->instance->getJointOutputs();
    return {v.data(), v.size()};
}

rust::Slice<const float> rig_channel_outputs(const Rig& rig) {
    const auto v = rig.impl->instance->getBlendShapeOutputs();
    return {v.data(), v.size()};
}

rust::Slice<const float> rig_map_outputs(const Rig& rig) {
    const auto v = rig.impl->instance->getAnimatedMapOutputs();
    return {v.data(), v.size()};
}

}  // namespace slerp_dna
