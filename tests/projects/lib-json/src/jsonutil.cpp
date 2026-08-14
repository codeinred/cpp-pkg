#include "jsonutil/jsonutil.hpp"

// Included via the PRIVATE include dir (src/detail) — bare name on purpose.
#include "version_detail.hpp"

#include <sstream>

namespace jsonutil {

nlohmann::json parse(const std::string& text) {
    return nlohmann::json::parse(text);
}

nlohmann::json merge(const nlohmann::json& base, const nlohmann::json& patch) {
    nlohmann::json out = base;
    out.merge_patch(patch);
    return out;
}

std::string describe(const nlohmann::json& value) {
    std::ostringstream os;
    os << value.type_name() << " with " << value.size() << " element(s)";
    return os.str();
}

std::string version() {
    return JSONUTIL_VERSION_STRING;
}

}  // namespace jsonutil
