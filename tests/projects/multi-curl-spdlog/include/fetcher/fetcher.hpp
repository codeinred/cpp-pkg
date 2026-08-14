// fetcher: wraps libcurl behind a curl-free interface. curl is a PRIVATE
// dependency of the fetcher static library: this header must not include
// <curl/curl.h>, consumers never see curl's headers/defines, yet the final
// executable link must still receive libcurl and its macOS frameworks
// (LINK_ONLY propagation through the static library).
#pragma once

#include <string>
#include <vector>

namespace fetcher {

// curl_version() as a std::string, e.g. "libcurl/8.15.0 SecureTransport ...".
std::string libcurl_version();

// Protocols compiled into libcurl (from curl_version_info).
std::vector<std::string> supported_protocols();

// Result of a network-free libcurl exercise: create an easy handle, set a
// URL option, URL-escape a string, tear everything down.
struct ProbeResult {
    bool handle_ok = false;         // curl_easy_init succeeded
    bool setopt_ok = false;         // CURLOPT_URL accepted
    std::string escaped;            // curl_easy_escape("hello world & more")
};

ProbeResult probe();

}  // namespace fetcher
