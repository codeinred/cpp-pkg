# multi-curl-spdlog

Multi-target project exercising non-trivial dependency extraction and
visibility propagation:

- **fmt 11.2.0** (git tag `11.2.0`).
- **spdlog v1.15.3** with `SPDLOG_FMT_EXTERNAL=ON` and `needs = ["fmt"]` —
  spdlog's installed `spdlogConfig.cmake` runs a real
  `find_dependency(fmt)`, so probing it tests transitive config loading and
  namespace attribution (fmt targets appear in spdlog's probe too).
- **curl 8.14.1** (git tag `curl-8_14_1`) built as a **static** libcurl with
  Secure Transport TLS. 8.14.1 is deliberate: Secure Transport was REMOVED
  in curl 8.15.0, where `CURL_USE_SECTRANSP=ON` becomes a silently unused
  cache var and configure falls back to whatever OpenSSL it can find
  (Homebrew — not hermetic). Statically, libcurl drags macOS frameworks
  (Security, SystemConfiguration, CoreFoundation, CoreServices) and system
  `-lz` into consumers' link lines — the LINK_ONLY/framework propagation
  stress test.

  The canonical consumer name `CURL::libcurl` is an ALIAS created inside
  `CURLConfig.cmake` (`add_library(CURL::libcurl ALIAS CURL::libcurl_static)`);
  alias targets never appear in the `IMPORTED_TARGETS` directory property,
  so the probe manifest exports the real imported target
  **`CURL::libcurl_static`**, which is what this project references.

## Final curl option set

`BUILD_CURL_EXE=OFF BUILD_SHARED_LIBS=OFF BUILD_STATIC_LIBS=ON
BUILD_LIBCURL_DOCS=OFF BUILD_MISC_DOCS=OFF BUILD_TESTING=OFF
BUILD_EXAMPLES=OFF CURL_DISABLE_LDAP=ON CURL_USE_LIBPSL=OFF
CURL_USE_LIBSSH2=OFF CURL_BROTLI=OFF CURL_ZSTD=OFF USE_NGHTTP2=OFF
USE_LIBIDN2=OFF CURL_USE_SECTRANSP=ON`

Everything Homebrew could opportunistically satisfy (psl, ssh2, brotli, zstd,
nghttp2, idn2) is off so the store artifact never links brew dylibs; TLS is
the OS-provided Secure Transport framework, so HTTPS support is compiled in
hermetically.

## Targets

| target | kind | deps | what it tests |
|---|---|---|---|
| `netlog` | static-library | **public** `spdlog::spdlog` | compile-requirement propagation to consumers |
| `fetcher` | static-library | **private** `CURL::libcurl_static` | consumers see no curl headers, but libcurl + frameworks still reach the final link (LINK_ONLY) |
| `crawler` | executable | `netlog`, `fetcher` | uses spdlog APIs it only receives via netlog; uses libcurl only through fetcher's curl-free API |
| `version-dump` | executable | `CURL::libcurl_static`, `spdlog::spdlog` | direct side-by-side consumption of both extracted deps |

## Expected output

`./build/crawler` (exit 0):

```
[crawler] [info] crawler starting (spdlog 1.15.3, fmt 110200)
[crawler] [info] libcurl: libcurl/8.14.1-DEV SecureTransport zlib/1.2.12
[crawler] [info] protocols: dict,file,ftp,...,https,... (N total)
[crawler] [info] https supported: yes
[crawler] [info] probe: handle_ok=true setopt_ok=true escaped='hello%20world%20%26%20more'
[crawler] [info] CRAWLER OK
```

`./build/version-dump` (exit 0):

```
curl_version: libcurl/8.14.1-DEV SecureTransport zlib/1.2.12
spdlog: 1.15.3
libcurl_version_num: 0x080e01
```

(The `-DEV` suffix is curl's marker for builds from a git checkout instead
of a release tarball; the pinned commit is exactly the `curl-8_14_1` tag.)

A second `cpp-pkg build` must be a store cache hit (no dependency rebuild —
no `building dependency` lines; project targets go to `ninja: no work to do`).
