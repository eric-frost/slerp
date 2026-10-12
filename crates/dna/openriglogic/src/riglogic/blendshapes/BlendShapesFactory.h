// Copyright Epic Games, Inc. All Rights Reserved.

#pragma once

#include "riglogic/TypeDefs.h"
#include "riglogic/blendshapes/BlendShapes.h"

namespace rl4 {

struct Configuration;
struct RigMetadata;
class Controls;

struct BlendShapesFactory {
    static BlendShapes::Pointer
    create(const Configuration& config, RigMetadata* meta, const dna::Reader* reader, Controls* controls, MemoryResource* memRes);
    static BlendShapes::Pointer create(const Configuration& config, RigMetadata* meta, MemoryResource* memRes);
};

}  // namespace rl4
