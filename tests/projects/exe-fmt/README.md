# exe-fmt

Single executable consuming the real **fmt** library
(https://github.com/fmtlib/fmt, tag `11.2.0`) as a git dependency.

Exercises: git-tag fetch, CMake dependency build in the store, tier-2 probe
of `fmt-config.cmake` (exported target `fmt::fmt`), manifest extraction,
link, and run. Second `cpp-pkg build` must be a store cache hit (no
dependency rebuild).

Build and run (from this directory):

```
cpp-pkg build
./build/exe-fmt
```

Expected output:

```
fmt version: 110200
Hello, world! pi ~ 3.142
hex: 0xbeef | padded: 000042 | centered: ****fmt****
primes: [2, 3, 5, 7, 11, 13]
sum of 6 primes = 41
```
