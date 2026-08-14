# provider-consumer — CMake dependency-provider mode

Exercises cpp-pkg's SECONDARY mode: the consumer's own build stays **CMake**
(`CMakeLists.txt`, `find_package(fmt REQUIRED)`, links `fmt::fmt`), and cpp-pkg
acts only as a CMake >= 3.24 dependency provider. `CppPkg.toml` (no `[targets]`)
declares `fmt = { git = ..., tag = "11.2.0" }`; the provider script shells out
to `cpp-pkg provide`, which fetches/builds/probes fmt into the store, emits a
`fmtConfig.cmake` shim, and prints the shim dir that the nested `find_package`
consumes.

## How to run

```sh
# 1. Generate cppkg_provider.cmake with the cpp-pkg binary path baked in.
#    (Generated file is machine-specific — do not commit it.)
./setup-provider.sh /path/to/cpp-pkg      # or bare, if cpp-pkg is on PATH

# 2. Configure + build + run. CMAKE_BUILD_TYPE=Release maps to cpp-pkg's
#    `release` config (the provider lowercases it; empty also means release).
cmake -S . -B build -G Ninja -DCMAKE_BUILD_TYPE=Release \
      -DCMAKE_PROJECT_TOP_LEVEL_INCLUDES=$PWD/cppkg_provider.cmake
cmake --build build
./build/provider-consumer
```

(Optionally set `CPPKG_STORE=<dir>` in the environment of the `cmake -S`
configure to redirect the store; `execute_process` inherits it.)

## Expected output

```
hello from provider-consumer via cpp-pkg provider mode
first then second
primes: [2, 3, 5, 7, 11]
duration: 1500ms
FMT_VERSION=110200
```

## What is verified

- First configure builds fmt 11.2.0 from source into the store (`pkg/fmt-<hash>`)
  and writes `CppPkg.lock` (committed here; pins commit
  `40626af88bd7df9a5fb80be7b25ac85b122d6c21`).
- `fmt_DIR` in `build/CMakeCache.txt` and the link line in `build/build.ninja`
  point into the store (`.../store/pkg/fmt-<hash>/install/lib/libfmt.a`).
- A clean reconfigure (`rm -rf build`, same configure) is a store cache hit:
  sub-second, no dependency rebuild (store artifact mtimes unchanged).

## Provider script emission

`setup-provider.sh` is a thin wrapper over `cpp-pkg provider-script --dir .`,
which emits `cppkg_provider.cmake` with the invoking binary's absolute path
baked in. The generated file is machine-specific: regenerate it, don't commit
it.
