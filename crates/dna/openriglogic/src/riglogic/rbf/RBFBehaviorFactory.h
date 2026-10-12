// Copyright Epic Games, Inc. All Rights Reserved.

#pragma once

#include "riglogic/TypeDefs.h"
#include "riglogic/rbf/RBFBehavior.h"

namespace rl4 {

struct Configuration;
struct RigMetadata;

struct RBFBehaviorFactory {
    static RBFBehavior::Pointer create(const Configuration& config,
                                       RigMetadata* meta,
                                       const dna::Reader* reader,
                                       MemoryResource* memRes);
    static RBFBehavior::Pointer create(const Configuration& config, RigMetadata* meta, MemoryResource* memRes);
};

}  // namespace rl4
