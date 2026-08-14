#include "fetcher/fetcher.hpp"

#include <curl/curl.h>

namespace fetcher {

std::string libcurl_version() {
    return curl_version();
}

std::vector<std::string> supported_protocols() {
    std::vector<std::string> out;
    const curl_version_info_data* info = curl_version_info(CURLVERSION_NOW);
    if (info != nullptr && info->protocols != nullptr) {
        for (const char* const* p = info->protocols; *p != nullptr; ++p) {
            out.emplace_back(*p);
        }
    }
    return out;
}

ProbeResult probe() {
    ProbeResult result;
    // Network-free libcurl exercise: global init, an easy handle, one
    // setopt, and the URL-escaping API. Proves we linked a functioning
    // libcurl (and, statically, its Secure Transport/zlib underpinnings).
    curl_global_init(CURL_GLOBAL_DEFAULT);
    CURL* handle = curl_easy_init();
    result.handle_ok = handle != nullptr;
    if (handle != nullptr) {
        result.setopt_ok =
            curl_easy_setopt(handle, CURLOPT_URL, "https://example.com/") ==
            CURLE_OK;
        char* escaped =
            curl_easy_escape(handle, "hello world & more", 0);
        if (escaped != nullptr) {
            result.escaped = escaped;
            curl_free(escaped);
        }
        curl_easy_cleanup(handle);
    }
    curl_global_cleanup();
    return result;
}

}  // namespace fetcher
