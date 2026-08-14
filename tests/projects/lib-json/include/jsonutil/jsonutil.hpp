#pragma once

// Public header: deliberately includes nlohmann/json.hpp so that any consumer
// of jsonutil needs nlohmann_json's include paths on its own compile line —
// this is what makes the PUBLIC dependency edge load-bearing.
#include <nlohmann/json.hpp>

#include <string>

namespace jsonutil {

// Parse `text`, returning the parsed object; throws nlohmann::json exceptions
// on malformed input.
nlohmann::json parse(const std::string& text);

// Deep-merge `patch` into `base` (RFC 7386 merge-patch) and return the result.
nlohmann::json merge(const nlohmann::json& base, const nlohmann::json& patch);

// Render a one-line summary "<type> with N elements" style description.
std::string describe(const nlohmann::json& value);

// Library version string (built from a private detail header).
std::string version();

}  // namespace jsonutil
