// Copyright Epic Games, Inc. All Rights Reserved.

#pragma once

#include "riglogic/TypeDefs.h"
#include "riglogic/ml/MachineLearnedBehavior.h"

namespace rl4 {

struct Configuration;
struct RigMetadata;

struct MachineLearnedBehaviorFactory {
    static MachineLearnedBehavior::Pointer create(const Configuration& config,
                                                  RigMetadata* meta,
                                                  const dna::Reader* reader,
                                                  MemoryResource* memRes);
    static MachineLearnedBehavior::Pointer create(const Configuration& config, RigMetadata* meta, MemoryResource* memRes);
};

}  // namespace rl4
