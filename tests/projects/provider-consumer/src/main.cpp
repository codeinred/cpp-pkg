// Exercises fmt as a COMPILED library (not header-only): fmt::format,
// argument reordering, chrono formatting, and fmt::join all route through
// libfmt.a from the cpp-pkg store via the imported fmt::fmt shim target.
#include <fmt/base.h>
#include <fmt/chrono.h>
#include <fmt/format.h>
#include <fmt/ranges.h>

#include <chrono>
#include <vector>

int main() {
    fmt::print("hello from {} via {}\n", "provider-consumer", "cpp-pkg provider mode");

    // Positional-argument reordering.
    fmt::print("{1} then {0}\n", "second", "first");

    // fmt/ranges: join a vector with a separator.
    std::vector<int> primes{2, 3, 5, 7, 11};
    fmt::print("primes: [{}]\n", fmt::join(primes, ", "));

    // fmt/chrono: duration formatting.
    using namespace std::chrono_literals;
    fmt::print("duration: {}\n", 1500ms);

    fmt::print("FMT_VERSION={}\n", FMT_VERSION);
    return 0;
}
