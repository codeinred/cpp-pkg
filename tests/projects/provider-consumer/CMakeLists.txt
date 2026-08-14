# Consumer built by CMake itself; fmt is resolved by the cpp-pkg dependency
# provider (CMake >= 3.24 SET_DEPENDENCY_PROVIDER), injected via
# -DCMAKE_PROJECT_TOP_LEVEL_INCLUDES=<path to cppkg_provider.cmake>.
cmake_minimum_required(VERSION 3.24)
project(provider_consumer CXX)

find_package(fmt REQUIRED)

add_executable(provider-consumer src/main.cpp)
target_link_libraries(provider-consumer PRIVATE fmt::fmt)
