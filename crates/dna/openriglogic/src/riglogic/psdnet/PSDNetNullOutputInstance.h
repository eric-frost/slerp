// Copyright Epic Games, Inc. All Rights Reserved.

#pragma once

#include "riglogic/TypeDefs.h"
#include "riglogic/psdnet/PSDNetOutputInstance.h"

#include <cstdint>

namespace rl4 {

class PSDNetNullOutputInstance : public PSDNetOutputInstance {
public:
    void resetClampBuffer() override;
    ArrayView<float> getClampBuffer() override;
};

}  // namespace rl4
