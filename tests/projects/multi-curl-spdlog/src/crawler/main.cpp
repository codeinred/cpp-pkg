// crawler: consumes both project libraries.
//  - netlog (PUBLIC spdlog): we call spdlog APIs directly here, using headers
//    that only propagation through netlog can provide (this file's target
//    does not depend on spdlog itself).
//  - fetcher (PRIVATE curl): we exercise libcurl only through fetcher's
//    curl-free API; libcurl reaches our link line via LINK_ONLY propagation.
#include <fetcher/fetcher.hpp>
#include <netlog/netlog.hpp>

// Both reach us only via netlog's PUBLIC spdlog edge (spdlog was built with
// SPDLOG_FMT_EXTERNAL=ON, so external fmt headers propagate through it).
#include <fmt/ranges.h>      // fmt::join
#include <spdlog/fmt/fmt.h>

#include <algorithm>
#include <string>

int main() {
    auto log = netlog::make_logger("crawler");

    log->info("crawler starting (spdlog {}.{}.{}, fmt {})", SPDLOG_VER_MAJOR,
              SPDLOG_VER_MINOR, SPDLOG_VER_PATCH, FMT_VERSION);

    log->info("libcurl: {}", fetcher::libcurl_version());

    auto protocols = fetcher::supported_protocols();
    const bool has_https =
        std::find(protocols.begin(), protocols.end(), "https") !=
        protocols.end();
    log->info("protocols: {} ({} total)", fmt::join(protocols, ","),
              protocols.size());
    log->info("https supported: {}", has_https ? "yes" : "no");

    auto probe = fetcher::probe();
    log->info("probe: handle_ok={} setopt_ok={} escaped='{}'",
              probe.handle_ok, probe.setopt_ok, probe.escaped);

    const bool ok = probe.handle_ok && probe.setopt_ok &&
                    probe.escaped == "hello%20world%20%26%20more" && has_https;
    if (!ok) {
        log->error("CRAWLER FAILED");
        return 1;
    }
    log->info("CRAWLER OK");
    return 0;
}
