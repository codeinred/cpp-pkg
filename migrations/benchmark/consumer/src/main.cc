// Smoke consumer for benchmark::benchmark_main pulled in via cpp-pkg
// extraction. main() comes from libbenchmark_main.a; if the PUBLIC edge
// benchmark_main -> benchmark did not propagate includes/defines/link
// artifacts, this TU would fail to compile or link.
#include <benchmark/benchmark.h>

#include <string>
#include <vector>

static void BM_StringAppend(benchmark::State& state) {
  for (auto _ : state) {
    std::string s;
    for (int i = 0; i < static_cast<int>(state.range(0)); ++i) {
      s += "x";
    }
    benchmark::DoNotOptimize(s);
  }
}
BENCHMARK(BM_StringAppend)->Arg(64)->Arg(512);

static void BM_VectorPushBack(benchmark::State& state) {
  for (auto _ : state) {
    std::vector<int> v;
    v.reserve(static_cast<size_t>(state.range(0)));
    for (int i = 0; i < static_cast<int>(state.range(0)); ++i) {
      v.push_back(i);
    }
    benchmark::DoNotOptimize(v.data());
  }
}
BENCHMARK(BM_VectorPushBack)->Arg(1024);
