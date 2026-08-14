// Demo exe exercising absl::StrCat, absl::StrFormat, absl::flat_hash_map.
// Output is deterministic (keys sorted before printing) so it can be
// byte-compared between the cpp-pkg build and the upstream-CMake build.
#include <algorithm>
#include <cstdio>
#include <string>
#include <vector>

#include "absl/container/flat_hash_map.h"
#include "absl/strings/str_cat.h"
#include "absl/strings/str_format.h"

int main() {
  absl::flat_hash_map<std::string, int> counts;
  const char* words[] = {"pack", "my", "box", "with", "five",
                         "dozen", "liquor", "jugs", "box", "my",
                         "my",   "pack"};
  for (const char* w : words) ++counts[absl::StrCat("w_", w)];

  std::vector<std::string> keys;
  keys.reserve(counts.size());
  for (const auto& kv : counts) keys.push_back(kv.first);
  std::sort(keys.begin(), keys.end());

  std::string out = absl::StrCat("distinct=", counts.size(), "\n");
  for (const auto& k : keys) {
    absl::StrAppendFormat(&out, "%-12s : %03d\n", k, counts.at(k));
  }
  out += absl::StrFormat("pi=%.5f hex=%#x str=%s\n", 3.14159265, 48879,
                         absl::StrCat("cat", "-", 42));
  std::fputs(out.c_str(), stdout);
  return 0;
}
