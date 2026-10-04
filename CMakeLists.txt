cmake_minimum_required(VERSION 3.25)
project(origami LANGUAGES NONE)

if(CMAKE_SOURCE_DIR STREQUAL CMAKE_BINARY_DIR)
  message(FATAL_ERROR "Select an out-of-tree build directory with -B or a preset")
endif()
find_package(Git REQUIRED)
add_custom_target(submodules
  COMMAND "${GIT_EXECUTABLE}" -C "${CMAKE_SOURCE_DIR}" submodule update --init -- qemu instigator
  COMMAND "${CMAKE_COMMAND}" -S "${CMAKE_SOURCE_DIR}" -B "${CMAKE_BINARY_DIR}"
  COMMENT "Initialize pinned dependencies and configure the product"
  VERBATIM)
if(NOT EXISTS "${CMAKE_SOURCE_DIR}/qemu/configure" OR NOT EXISTS "${CMAKE_SOURCE_DIR}/instigator/go.mod")
  message(STATUS "Initialize dependencies: cmake --build \"${CMAKE_BINARY_DIR}\" --target submodules")
  add_custom_target(product ALL
    COMMAND "${CMAKE_COMMAND}" -E echo "Run cmake --build \"${CMAKE_BINARY_DIR}\" --target submodules first"
    COMMAND "${CMAKE_COMMAND}" -E false VERBATIM)
  return()
endif()

include(ExternalProject)
include(CTest)
find_package(Python3 3.11 REQUIRED COMPONENTS Interpreter)
find_program(PRODUCT_MAKE NAMES gmake make REQUIRED)
find_program(PRODUCT_CARGO cargo REQUIRED)
find_program(PRODUCT_RUSTC rustc REQUIRED)
find_program(PRODUCT_GO go REQUIRED)
set(CMAKE_BUILD_TYPE Release CACHE STRING "Cargo configuration: Debug or Release")
set(PRODUCT_PLATFORM "" CACHE STRING "Product platform: linux, macos or windows")
set(PRODUCT_RUST_TARGET "" CACHE STRING "Cargo target for cross compilation")
set(PRODUCT_CROSS_PREFIX "" CACHE STRING "QEMU cross compiler prefix")
set(PRODUCT_JOBS 4 CACHE STRING "Maximum parallel QEMU compile jobs")
if(NOT PRODUCT_PLATFORM)
  if(CMAKE_HOST_APPLE)
    set(PRODUCT_PLATFORM macos)
  elseif(CMAKE_HOST_WIN32)
    set(PRODUCT_PLATFORM windows)
  else()
    set(PRODUCT_PLATFORM linux)
  endif()
endif()
if(NOT PRODUCT_PLATFORM MATCHES "^(linux|macos|windows)$")
  message(FATAL_ERROR "Unsupported PRODUCT_PLATFORM: ${PRODUCT_PLATFORM}")
endif()
if(CMAKE_BUILD_TYPE STREQUAL "Release")
  set(build_type release)
  set(cargo_flags --release)
elseif(CMAKE_BUILD_TYPE STREQUAL "Debug")
  set(build_type debug)
else()
  message(FATAL_ERROR "CMAKE_BUILD_TYPE must be Debug or Release")
endif()
set(exe "")
set(go_platform "")
if(PRODUCT_PLATFORM STREQUAL "linux" AND NOT CMAKE_HOST_SYSTEM_PROCESSOR MATCHES "^(x86_64|AMD64)$")
  message(FATAL_ERROR "Linux product builds require an x86-64 host")
endif()
if(PRODUCT_PLATFORM STREQUAL "windows" AND CMAKE_HOST_WIN32)
  message(FATAL_ERROR "Native Windows packaging is not supported; use the windows-cross preset in the Linux build container")
endif()
if(PRODUCT_PLATFORM STREQUAL "macos" AND (NOT CMAKE_HOST_APPLE OR NOT CMAKE_HOST_SYSTEM_PROCESSOR STREQUAL "arm64"))
  message(FATAL_ERROR "macOS builds require a native arm64 Mac")
endif()
if(PRODUCT_PLATFORM STREQUAL "windows")
  set(exe .exe)
  set(go_platform GOOS=windows GOARCH=amd64)
endif()
set(qemu_build "${CMAKE_BINARY_DIR}/qemu-build")
set(cargo_target_dir "${CMAKE_BINARY_DIR}/cargo-target")
set(cargo_home "${CMAKE_BINARY_DIR}/cargo-home")
set(go_mod_cache "${CMAKE_BINARY_DIR}/go-mod")
set(go_build_cache "${CMAKE_BINARY_DIR}/go-build")
set(instigator_binary "${CMAKE_BINARY_DIR}/instigator${exe}")
set(output_dir "${CMAKE_BINARY_DIR}/run")
set(frontend_binary "${cargo_target_dir}")
if(PRODUCT_RUST_TARGET)
  string(APPEND frontend_binary "/${PRODUCT_RUST_TARGET}")
endif()
string(APPEND frontend_binary "/${build_type}/origami${exe}")
set(cargo_environment "CARGO_TARGET_DIR=${cargo_target_dir}" "CARGO_HOME=${cargo_home}")
set(go_environment "GOPATH=${CMAKE_BINARY_DIR}/go" "GOMODCACHE=${go_mod_cache}" "GOCACHE=${go_build_cache}" CGO_ENABLED=0 GOTOOLCHAIN=auto "XDG_CONFIG_HOME=${CMAKE_BINARY_DIR}/tool-config")
set(qemu_flags --target-list=mips64-softmmu --disable-docs --disable-gtk --disable-cocoa --enable-sdl --enable-vnc --enable-slirp)
if(PRODUCT_CROSS_PREFIX)
  list(APPEND qemu_flags "--cross-prefix=${PRODUCT_CROSS_PREFIX}")
endif()
if(PRODUCT_RUST_TARGET)
  list(APPEND cargo_flags --target "${PRODUCT_RUST_TARGET}")
  if(PRODUCT_RUST_TARGET STREQUAL "x86_64-pc-windows-gnu")
    list(APPEND cargo_environment "CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=${PRODUCT_CROSS_PREFIX}gcc")
  endif()
endif()
set(platform_environment "MESON_PACKAGE_CACHE_DIR=${CMAKE_BINARY_DIR}/meson-package-cache")
if(PRODUCT_PLATFORM STREQUAL "macos")
  list(APPEND platform_environment MACOSX_DEPLOYMENT_TARGET=15.0)
endif()
set(qemu_configure COMMAND "${CMAKE_COMMAND}" -E env ${platform_environment}
  "${CMAKE_SOURCE_DIR}/qemu/configure" ${qemu_flags})
if(PRODUCT_PLATFORM STREQUAL "windows")
  find_program(PRODUCT_MESON meson REQUIRED)
  list(APPEND qemu_configure COMMAND "${PRODUCT_MESON}" configure
    -Dc_args=-DBUILDING_LIBSLIRP -Dcurl=disabled -Dgnutls=disabled -Dgcrypt=disabled
    -Dbzip2=disabled -Dlibssh=disabled -Dlibnfs=disabled -Dlibiscsi=disabled
    -Drbd=disabled -Dvde=disabled -Dspice=disabled -Dopengl=disabled -Dvirglrenderer=disabled)
endif()
# Native tools own source dependency tracking and compiler caches.
ExternalProject_Add(qemu-product
  SOURCE_DIR "${CMAKE_SOURCE_DIR}/qemu" BINARY_DIR "${qemu_build}"
  DOWNLOAD_COMMAND "" UPDATE_COMMAND "" PATCH_COMMAND ""
  CONFIGURE_COMMAND ${qemu_configure}
  BUILD_COMMAND "${CMAKE_COMMAND}" -E env ${platform_environment}
    "${Python3_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/build/stage-product.py"
      "${CMAKE_BINARY_DIR}/product-build.json" --prepare-qemu
    COMMAND "${CMAKE_COMMAND}" -E env ${platform_environment}
      "${PRODUCT_MAKE}" -j "${PRODUCT_JOBS}" "qemu-system-mips64${exe}" "qemu-img${exe}"
  BUILD_ALWAYS TRUE USES_TERMINAL_BUILD TRUE INSTALL_COMMAND ""
  BUILD_BYPRODUCTS "${qemu_build}/qemu-system-mips64${exe}" "${qemu_build}/qemu-img${exe}")
# Meson keeps extracted subprojects until explicitly updated.
file(GLOB libslirp_patches CONFIGURE_DEPENDS
  "${CMAKE_SOURCE_DIR}/qemu/subprojects/packagefiles/libslirp*.patch")
ExternalProject_Add_Step(qemu-product refresh-libslirp
  COMMAND "${CMAKE_COMMAND}" -E env ${platform_environment}
    "${qemu_build}/pyvenv/bin/meson" subprojects update --reset
    --sourcedir "${CMAKE_SOURCE_DIR}/qemu" libslirp
  WORKING_DIRECTORY "${qemu_build}"
  DEPENDEES configure DEPENDERS build
  DEPENDS "${CMAKE_SOURCE_DIR}/qemu/subprojects/libslirp.wrap" ${libslirp_patches})
add_custom_target(frontend
  COMMAND "${CMAKE_COMMAND}" -E env ${platform_environment} ${cargo_environment}
    "${PRODUCT_CARGO}" build --locked ${cargo_flags}
  WORKING_DIRECTORY "${CMAKE_SOURCE_DIR}" VERBATIM)
add_custom_target(instigator-product
  COMMAND "${CMAKE_COMMAND}" -E env ${go_environment} ${go_platform}
    "${PRODUCT_GO}" build -mod=readonly -trimpath -buildvcs=true -o "${instigator_binary}" ./cmd/instigator
  WORKING_DIRECTORY "${CMAKE_SOURCE_DIR}/instigator" VERBATIM)

if(PRODUCT_PLATFORM STREQUAL "macos")
  set(archive_root_name macos-arm64-dev)
  set(archive_name origami-macos-arm64-preview)
else()
  set(archive_root_name "${PRODUCT_PLATFORM}-dev")
  set(archive_name "origami-${PRODUCT_PLATFORM}-x86_64-preview")
endif()
# Source roots shared by compilation and packaging.
set(product_source "${CMAKE_SOURCE_DIR}")
set(qemu_source "${CMAKE_SOURCE_DIR}/qemu")
set(instigator_source "${CMAKE_SOURCE_DIR}/instigator")
set(rustc "${PRODUCT_RUSTC}")
set(go "${PRODUCT_GO}")
set(python "${Python3_EXECUTABLE}")
set(platform "${PRODUCT_PLATFORM}")
set(rust_target "${PRODUCT_RUST_TARGET}")
# The generated manifest supplies packaging with the actual build inputs.
function(json_string result value)
  string(REPLACE "\\" "\\\\" value "${value}")
  string(REPLACE "\"" "\\\"" value "${value}")
  set(${result} "\"${value}\"" PARENT_SCOPE)
endfunction()
set(manifest "{}")
foreach(key product_source qemu_source instigator_source qemu_build cargo_target_dir cargo_home go_mod_cache go_build_cache rustc go python platform rust_target output_dir)
  json_string(value "${${key}}")
  string(JSON manifest SET "${manifest}" "${key}" "${value}")
endforeach()
set(environment "[]")
foreach(entry IN LISTS go_environment go_platform)
  json_string(value "${entry}")
  string(JSON length LENGTH "${environment}")
  string(JSON environment SET "${environment}" ${length} "${value}")
endforeach()
string(JSON manifest SET "${manifest}" go_environment "${environment}")
set(files "{}")
function(product_artifact destination source)
  json_string(value "${source}")
  string(JSON files SET "${files}" "${destination}" "${value}")
  set(files "${files}" PARENT_SCOPE)
endfunction()
product_artifact("bin/origami${exe}" "${frontend_binary}")
product_artifact("bin/instigator${exe}" "${instigator_binary}")
product_artifact("libexec/sgi/qemu-system-mips64${exe}" "${qemu_build}/qemu-system-mips64${exe}")
product_artifact("libexec/sgi/qemu-img${exe}" "${qemu_build}/qemu-img${exe}")
if(NOT IS_DIRECTORY "${qemu_source}/pc-bios/keymaps")
  message(FATAL_ERROR "Missing QEMU keymaps: ${qemu_source}/pc-bios/keymaps")
endif()
file(GLOB keymaps CONFIGURE_DEPENDS "${qemu_source}/pc-bios/keymaps/*")
foreach(source IN LISTS keymaps)
  get_filename_component(name "${source}" NAME)
  if(NOT IS_DIRECTORY "${source}" AND NOT name STREQUAL "meson.build")
    product_artifact("share/sgi/qemu/keymaps/${name}" "${source}")
  endif()
endforeach()
string(JSON manifest SET "${manifest}" files "${files}")
file(CONFIGURE OUTPUT "${CMAKE_BINARY_DIR}/product-build.json" CONTENT "${manifest}\n" @ONLY)
add_custom_target(binaries DEPENDS qemu-product frontend instigator-product)
add_custom_target(product ALL
  COMMAND "${Python3_EXECUTABLE}" "${CMAKE_SOURCE_DIR}/build/stage-product.py"
    "${CMAKE_BINARY_DIR}/product-build.json"
  DEPENDS binaries VERBATIM)

if(BUILD_TESTING)
  # Cross products are tested with native tools; target executables cannot run here.
  add_test(NAME frontend COMMAND "${CMAKE_COMMAND}" -E env ${cargo_environment}
    "${PRODUCT_CARGO}" test --locked)
  set_tests_properties(frontend PROPERTIES WORKING_DIRECTORY "${CMAKE_SOURCE_DIR}")
  add_test(NAME instigator COMMAND "${CMAKE_COMMAND}" -E env ${go_environment}
    "${PRODUCT_GO}" test -mod=readonly ./internal/qemunet ./cmd/instigator)
  set_tests_properties(instigator PROPERTIES WORKING_DIRECTORY "${CMAKE_SOURCE_DIR}/instigator")
  file(MAKE_DIRECTORY "${CMAKE_BINARY_DIR}/test-tmp")
  add_test(NAME build-helpers COMMAND "${Python3_EXECUTABLE}" -m unittest discover -s "${CMAKE_SOURCE_DIR}/build/tests")
  set_tests_properties(build-helpers PROPERTIES ENVIRONMENT "TMPDIR=${CMAKE_BINARY_DIR}/test-tmp;CARGO_HOME=${cargo_home}")
endif()
configure_file(cmake/install-product.cmake.in "${CMAKE_BINARY_DIR}/install-product.cmake" @ONLY)
install(SCRIPT "${CMAKE_BINARY_DIR}/install-product.cmake")
set(CPACK_PACKAGE_FILE_NAME "${archive_name}")
set(CPACK_INCLUDE_TOPLEVEL_DIRECTORY OFF)
set(CPACK_PACKAGING_INSTALL_PREFIX "/")
set(CPACK_PACKAGE_CHECKSUM SHA256)
if(PRODUCT_PLATFORM STREQUAL "windows")
  set(CPACK_GENERATOR ZIP)
else()
  set(CPACK_GENERATOR TGZ)
endif()
configure_file(cmake/package-version.cmake.in "${CMAKE_BINARY_DIR}/package-version.cmake" @ONLY)
set(CPACK_PROJECT_CONFIG_FILE "${CMAKE_BINARY_DIR}/package-version.cmake")
include(CPack)
