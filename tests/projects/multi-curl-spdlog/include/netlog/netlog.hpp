// netlog: thin logging facade over spdlog. spdlog is a PUBLIC dependency of
// the netlog static library, so this header may include spdlog headers and
// every consumer of netlog gets spdlog's usage requirements transitively.
#pragma once

#include <spdlog/spdlog.h>

#include <memory>
#include <string>

namespace netlog {

// Returns a stdout logger with a deterministic (timestamp-free) pattern so
// test output is stable: "[<name>] [<level>] <message>".
std::shared_ptr<spdlog::logger> make_logger(const std::string& name);

}  // namespace netlog
