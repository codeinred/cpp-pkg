#pragma once

// PRIVATE detail header. It lives under src/detail, which is a private
// include dir of jsonutil — this path must never appear on a consumer's
// compile line (verified in the test via `cpp-pkg build --query`).
#define JSONUTIL_VERSION_STRING "jsonutil/0.1.0 (private-detail)"
