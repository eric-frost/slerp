// Copyright Epic Games, Inc. All Rights Reserved.

#pragma once

#include "riglogic/TypeDefs.h"
#include "riglogic/animatedmaps/AnimatedMaps.h"

namespace rl4 {

struct Configuration;
struct RigMetadata;
class Controls;

struct AnimatedMapsFactory {
    static AnimatedMaps::Pointer
    create(const Configuration& config, RigMetadata* meta, const dna::Reader* reader, Controls* controls, MemoryResource* memRes);
    static AnimatedMaps::Pointer create(const Configuration& config, RigMetadata* meta, MemoryResource* memRes);
};

}  // namespace rl4
