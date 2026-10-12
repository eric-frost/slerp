// Copyright Epic Games, Inc. All Rights Reserved.

#pragma once

#include "riglogic/TypeDefs.h"

namespace rl4 {

class PSDNetOutputInstance {
public:
    using Pointer = UniqueInstance<PSDNetOutputInstance>::PointerType;
    using Factory = std::function<Pointer(MemoryResource*)>;

public:
    virtual ~PSDNetOutputInstance();
    virtual void resetClampBuffer() = 0;
    virtual ArrayView<float> getClampBuffer() = 0;
};

}  // namespace rl4
