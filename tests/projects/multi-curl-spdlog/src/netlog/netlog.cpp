#include "netlog/netlog.hpp"

#include <spdlog/sinks/stdout_sinks.h>

namespace netlog {

std::shared_ptr<spdlog::logger> make_logger(const std::string& name) {
    // Plain (non-color) stdout sink: deterministic bytes for output checks.
    auto sink = std::make_shared<spdlog::sinks::stdout_sink_mt>();
    auto logger = std::make_shared<spdlog::logger>(name, sink);
    logger->set_pattern("[%n] [%l] %v");
    logger->set_level(spdlog::level::debug);
    return logger;
}

}  // namespace netlog
