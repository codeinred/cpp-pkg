# CppPkg

CppPkg is a new package manager written in Rust. A project is specified
declaratively as a toml file, `CppPkg.toml`.

Where CppPkg distinguishes itself is in terms of it's ability to use and consume
projects written in CMake.

For every dependency it consumes, it generates a manifest file describing all of
the artifacts exposed by the dependency, which it can them consume to link
against libraries provided by the dependency.

1. If a dependency provides a Common Package Specification file on installation,
   this manifest is derivable from the Common Package Specification provided by
   the package.
2. If a dependency uses a `<project>Config.cmake` or another such file
   discoverable with `find_package`, CppPkg will generate a manifest based on
   the imported targets exposed by a call to `find_package`.
3. If a dependency is _not_ installable in the traditional sense (eg, the
   authors did not write install logic for it, and expect it to be consumed via
   FetchContent or `add_subdirectory`), then CMake is used to discover the
   _build system targets_ generated on configuration, and this is used to
   generate the manifest.

For each CMake package, CppPkg generates a minimal CMake project stub to (1)
identify the targets the package exposes, (2) probe them to discover details of
the exposed targets as necessary for the construction of the manifest.

For each target, this probe must discover the target's include directories; the
flags it exposes as part of it's interface for compilation or linking; macros
that the target defines and exposes as parts of it's interface; the name of the
library file on-disk (or the collection of object files, for an object library);
and other details of the target as necessary to construct the manifest.

Unlike Cargo, which can lean heavily on the existing `crates.io` package
repository, `CppPkg.toml` is, unfortunately, somewhat more manual.

Users specify which configuration options are given to compile a dependency
written in CMake, together with some form of URI which identifies the package.
This can be a github repo + tag, a github repo + a commit, a link to a zip file
or tarball, or something else that identifies the package.

They can then use the names of targets exported by the package when specifying
the dependencies of libraries, executables, or other artifacts built by CppPkg.

CppPkg retains a number of on-disk stores:

1. The raw download store - holds packages in their raw, downloaded form
2. The package artifact store - holds the installed form of the package (+ the
   manifest) for installed packages, or the package-as-a-consumable-subdir (so
   holds a source tree, a build tree, and a manifest).

The artifact store is content-addressed based on (1) the package being compiled,
as a hash, and (2) the configuration options used to compile it. This allows
multiple separate configurations of a package to live on the system side by
side.

In order to pin tags, and to quickly resolve packages, CppPkg generates a file
`CppPkg.lock`, which contains the information necessary to resolve a package and
discover it's manifest.

By default, CppPkg uses `./build` as the project build directory. It generates a
`build/compile_commands.json` within the build directory.

By default (for the initial version), CppPkg acts as the full build system -
`cpp-pkg build` should build the project in it's entirety.
`cpp-pkg build [target(s)]` builds a particular target within the project.
`cpp-pkg build [target(s)] --query` shows all of the compile commands used to
build the target(s), and `cpp-pkg build [target(s)] --query <path>` shows the
compile commands used to build the given translation unit for the given targets
(different targets may build a translation unit with different compile options).

If no targets are specified but a path is queried, the compile command for the
given path is shown across all targets in appears in.

For fast prototyping
`cpp-pkg build --path path/to/cpp/file.cpp --with <dep> <dep> <dep>` allows for
quickly building a particular file against the given dependencies, specified as
target names.

**Naming a target:** If a target name is unique across all dependencies, it can
be used directly. Eg, if `fmt::fmt` is a unique target name across all
dependencies, you can refer to `fmt::fmt` directly.

If a target is _not_ unique, but it's a target that begins with `<name>::`, it
belongs to the dependency whose name matches `<name>`. So if there's a `fmt`
dependency, `fmt::fmt` refers to the `fmt::fmt` target provided by the `fmt`
dependency.

A dependency may instead use `exposes_namespace = [...]`. Eg, if the dependency
was instead named `libfmt`, then it could use `exposes_namespace = ["fmt"]` to
declare ownership of all targets beginning with `fmt::`.

Alternatively, a dependency may explicitly declare ownership of explicit
targets: `exposes_targets = ["fmt::fmt"]` could be used to declare ownership of
the `fmt::fmt` target in particular.

`exposes_targets` may also be a mapping, which can be used to rename a target if
needed.
