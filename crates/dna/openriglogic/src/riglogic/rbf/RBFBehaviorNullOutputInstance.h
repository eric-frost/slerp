// Copyright Epic Games, Inc. All Rights Reserved.

#pragma once

#include "riglogic/rbf/RBFBehaviorOutputInstance.h"

namespace rl4 {

class RBFBehaviorNullOutputInstance : public RBFBehaviorOutputInstance {
public:
    ArrayView<float> getInputBuffer(std::uint16_t /*unused*/) override;
    ArrayView<float> getIntermediateWeightsBuffer(std::uint16_t /*unused*/) override;
    ArrayView<float> getOutputWeightsBuffer(std::uint16_t /*unused*/) override;
};

}  // namespace rl4
