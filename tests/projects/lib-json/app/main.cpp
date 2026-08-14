// The executable consumes ONLY jsonutil. It still uses nlohmann::json types
// directly (from jsonutil's public header), which only compiles if
// nlohmann_json's include dirs propagate transitively through jsonutil's
// PUBLIC dependency edge.
#include "jsonutil/jsonutil.hpp"

#include <iostream>

int main() {
    const nlohmann::json base =
        jsonutil::parse(R"({"name":"cpp-pkg","langs":["c++","rust"],"stars":1})");
    const nlohmann::json patch = jsonutil::parse(R"({"stars":42,"license":"MIT"})");

    const nlohmann::json merged = jsonutil::merge(base, patch);

    std::cout << "version: " << jsonutil::version() << "\n";
    std::cout << "merged: " << merged.dump() << "\n";
    std::cout << "describe: " << jsonutil::describe(merged) << "\n";
    std::cout << "langs[1]: " << merged["langs"][1].get<std::string>() << "\n";
    std::cout << "nlohmann version: " << NLOHMANN_JSON_VERSION_MAJOR << "."
              << NLOHMANN_JSON_VERSION_MINOR << "." << NLOHMANN_JSON_VERSION_PATCH
              << "\n";
    return 0;
}
