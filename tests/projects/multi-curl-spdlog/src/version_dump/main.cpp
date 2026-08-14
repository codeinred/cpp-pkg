// version-dump: tiny executable depending on CURL::libcurl and
// spdlog::spdlog DIRECTLY (no project libraries in between) — checks that an
// executable can consume extracted dependency targets side by side.
#include <curl/curl.h>
#include <spdlog/version.h>

#include <cstdio>

int main() {
    std::printf("curl_version: %s\n", curl_version());
    std::printf("spdlog: %d.%d.%d\n", SPDLOG_VER_MAJOR, SPDLOG_VER_MINOR,
                SPDLOG_VER_PATCH);
    std::printf("libcurl_version_num: 0x%06x\n", LIBCURL_VERSION_NUM);
    return 0;
}
