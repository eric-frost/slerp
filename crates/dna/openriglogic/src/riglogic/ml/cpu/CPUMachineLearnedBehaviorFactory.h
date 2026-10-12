// Copyright Epic Games, Inc. All Rights Reserved.

#pragma once

#include "riglogic/TypeDefs.h"
#include "riglogic/ml/MachineLearnedBehaviorEvaluator.h"

namespace rl4 {

struct Configuration;
struct RigMetadata;

namespace ml {

namespace cpu {

class Factory {
public:
    static MachineLearnedBehaviorEvaluator::Pointer create(const Configuration& config,
                                                           RigMetadata* meta,
                                                           const dna::Reader* reader,
                                                           MemoryResource* memRes);
};

}  // namespace cpu

}  // namespace ml

}  // namespace rl4
