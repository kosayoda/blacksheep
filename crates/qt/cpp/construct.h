#pragma once

#include <memory>

#include "blacksheep-qt/src/model.cxxqt.h"
#include "blacksheep-qt/src/shell.cxxqt.h"

namespace blacksheep {

// cxx cannot bind C++ constructors, so QObjects that Rust owns are created here.
inline std::unique_ptr<ListModel> new_list_model() { return std::make_unique<ListModel>(); }
inline std::unique_ptr<Shell> new_shell() { return std::make_unique<Shell>(); }

} // namespace blacksheep
