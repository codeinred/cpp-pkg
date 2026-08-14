# lib-json — library-centric project over a header-only (INTERFACE) dependency

Exercises:

- **Interface-kind extraction**: `nlohmann_json` v3.12.0 is a header-only
  package exporting the INTERFACE imported target
  `nlohmann_json::nlohmann_json`.
- **PUBLIC dependency propagation**: static library `jsonutil` declares
  `nlohmann_json::nlohmann_json` as a *public* dependency (its public header
  `include/jsonutil/jsonutil.hpp` includes `<nlohmann/json.hpp>`). The
  executable `jsondemo` declares **only** `jsonutil`; it still uses
  `nlohmann::json` types directly, so it only compiles if the json include
  path reaches it transitively.
- **PRIVATE include isolation**: `jsonutil` has a private include dir
  `src/detail` (used for `version_detail.hpp`). Verify with
  `cpp-pkg build --query app/main.cpp` that `src/detail` does NOT appear on
  the exe's compile line, while it DOES appear on `src/jsonutil.cpp`'s line.

## Build & run

```sh
CPPKG_STORE=<store dir> cpp-pkg build
./build/jsondemo
```

## Expected output

```
version: jsonutil/0.1.0 (private-detail)
merged: {"langs":["c++","rust"],"license":"MIT","name":"cpp-pkg","stars":42}
describe: object with 4 element(s)
langs[1]: rust
nlohmann version: 3.12.0
```
