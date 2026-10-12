// Copyright Epic Games, Inc. All Rights Reserved.

#include "riglogic/psdnet/PSDNetNullOutputInstance.h"

namespace rl4 {

ArrayView<float> PSDNetNullOutputInstance::getClampBuffer() {
    return {};
}

void PSDNetNullOutputInstance::resetClampBuffer() {
}

}  // namespace rl4
