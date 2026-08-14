// Exercises the fmt library non-trivially: positional/named args, numeric
// formatting, fmt::join over a container, chrono-free width/fill specs, and
// both fmt::format (string building) and fmt::print (direct stdout).
#include <fmt/core.h>
#include <fmt/format.h>
#include <fmt/ranges.h>

#include <string>
#include <vector>

int main() {
    // Version check proves we linked the pinned release (11.2.0 -> 110200).
    fmt::print("fmt version: {}\n", FMT_VERSION);

    // Positional and named arguments.
    std::string greeting =
        fmt::format("{1}, {0}! pi ~ {pi:.3f}", "world", "Hello",
                    fmt::arg("pi", 3.14159265));
    fmt::print("{}\n", greeting);

    // Numeric formatting: hex with prefix, zero-padded, fixed width/fill.
    fmt::print("hex: {0:#x} | padded: {1:06} | centered: {2:*^11}\n",
               48879, 42, "fmt");

    // fmt::join over a vector (fmt/ranges.h).
    std::vector<int> primes = {2, 3, 5, 7, 11, 13};
    fmt::print("primes: [{}]\n", fmt::join(primes, ", "));

    // Compile-time checked format string returning a built string.
    fmt::print("{}\n", fmt::format("sum of {} primes = {}", primes.size(),
                                   2 + 3 + 5 + 7 + 11 + 13));
    return 0;
}
