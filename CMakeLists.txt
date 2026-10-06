cmake_minimum_required(VERSION 3.25)
if(CMAKE_SOURCE_DIR STREQUAL CMAKE_BINARY_DIR)
  message(FATAL_ERROR "Select an out-of-tree build directory with -B or a preset")
endif()
# Checked before project(), which runs the vcpkg toolchain.
if(NOT EXISTS "${CMAKE_SOURCE_DIR}/qemu/configure" OR NOT EXISTS "${CMAKE_SOURCE_DIR}/instigator/go.mod"
    OR NOT EXISTS "${CMAKE_SOURCE_DIR}/vcpkg/scripts/buildsystems/vcpkg.cmake")
  message(FATAL_ERROR "Initialize the pinned dependencies first: git submodule update --init")
endif()
project(origami LANGUAGES NONE)
if(NOT VCPKG_INSTALLED_DIR OR NOT VCPKG_TARGET_TRIPLET)
  message(FATAL_ERROR "vcpkg's toolchain is not loaded, so QEMU's libraries cannot be built.\n"
    "Configure through a preset, for example: cmake --preset macos\n"
    "CMake applies a preset's toolchain only to a new build directory. A directory "
    "configured before vcpkg was added keeps its old settings; reconfigure it from "
    "scratch with: cmake --preset macos --fresh (or delete the build directory).")
endif()

include(ExternalProject)
include(CTest)
find_package(Git REQUIRED)
find_program(PRODUCT_MAKE NAMES gmake make REQUIRED)
find_program(PRODUCT_CARGO cargo REQUIRED)
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
set(instigator_binary "${CMAKE_BINARY_DIR}/instigator${exe}")
set(frontend_binary "${cargo_target_dir}")
if(PRODUCT_RUST_TARGET)
  string(APPEND frontend_binary "/${PRODUCT_RUST_TARGET}")
endif()
string(APPEND frontend_binary "/${build_type}/origami${exe}")
set(cargo_environment "CARGO_TARGET_DIR=${cargo_target_dir}" "CARGO_HOME=${CMAKE_BINARY_DIR}/cargo-home")
# Fedora's Go defaults to the local toolchain; Instigator's go.mod selects the version.
set(go_environment "GOPATH=${CMAKE_BINARY_DIR}/go" "GOMODCACHE=${CMAKE_BINARY_DIR}/go-mod"
  "GOCACHE=${CMAKE_BINARY_DIR}/go-build" CGO_ENABLED=0 GOTOOLCHAIN=auto
  "XDG_CONFIG_HOME=${CMAKE_BINARY_DIR}/tool-config")

# Require the display, graphics and audio support the product relies on so a
# missing dependency fails configuration instead of silently dropping it.
# Disable optional features the product does not use, so what the archive
# bundles does not depend on what else the build machine has installed.
set(qemu_flags --target-list=mips64-softmmu --disable-docs --disable-gtk --disable-cocoa
  --enable-sdl --enable-vnc --enable-slirp
  --disable-curl --disable-gnutls --disable-gcrypt --disable-bzip2 --disable-zstd
  --disable-libssh --disable-libnfs --disable-libiscsi --disable-rbd --disable-vde
  --disable-spice --disable-virglrenderer --disable-libusb --disable-opengl)
if(PRODUCT_PLATFORM STREQUAL "linux")
  # sndio is rarely installed on Linux hosts, and QEMU links every enabled driver.
  list(APPEND qemu_flags --enable-alsa --enable-pa --disable-sndio)
elseif(PRODUCT_PLATFORM STREQUAL "macos")
  list(APPEND qemu_flags --enable-coreaudio)
else()
  list(APPEND qemu_flags --enable-dsound --extra-cflags=-DBUILDING_LIBSLIRP)
endif()
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
# QEMU finds the libraries vcpkg built through pkg-config. Linux also needs
# the host's X11, ALSA, PulseAudio and udev. Elsewhere vcpkg is the
# only source, and the plain pkg-config command bypasses the MinGW wrapper's
# Fedora search path.
set(vcpkg_prefix "${VCPKG_INSTALLED_DIR}/${VCPKG_TARGET_TRIPLET}")
if(PRODUCT_PLATFORM STREQUAL "linux")
  list(APPEND platform_environment "PKG_CONFIG_PATH=${vcpkg_prefix}/lib/pkgconfig")
else()
  list(APPEND platform_environment PKG_CONFIG=pkg-config "PKG_CONFIG_LIBDIR=${vcpkg_prefix}/lib/pkgconfig")
endif()
if(PRODUCT_PLATFORM STREQUAL "macos")
  list(APPEND platform_environment MACOSX_DEPLOYMENT_TARGET=15.0)
endif()
set(qemu_configure COMMAND "${CMAKE_COMMAND}" -E env ${platform_environment}
  "${CMAKE_SOURCE_DIR}/qemu/configure" ${qemu_flags})
# Native tools own source dependency tracking and compiler caches.
ExternalProject_Add(qemu-product
  SOURCE_DIR "${CMAKE_SOURCE_DIR}/qemu" BINARY_DIR "${qemu_build}"
  CONFIGURE_COMMAND ${qemu_configure}
  BUILD_COMMAND "${CMAKE_COMMAND}" -DGIT_EXECUTABLE=${GIT_EXECUTABLE}
      "-DSOURCE=${CMAKE_SOURCE_DIR}/qemu" "-DBUILD=${qemu_build}"
      -P "${CMAKE_SOURCE_DIR}/cmake/qemu-version.cmake"
    COMMAND "${CMAKE_COMMAND}" -E env ${platform_environment}
      "${PRODUCT_MAKE}" -j "${PRODUCT_JOBS}" "qemu-system-mips64${exe}" "qemu-img${exe}"
  BUILD_ALWAYS TRUE USES_TERMINAL_BUILD TRUE INSTALL_COMMAND "")
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
add_custom_target(binaries DEPENDS qemu-product frontend instigator-product)
# The default build installs a runnable tree next to the build outputs.
add_custom_target(product ALL
  COMMAND "${CMAKE_COMMAND}" --install "${CMAKE_BINARY_DIR}" --prefix "${CMAKE_BINARY_DIR}/run"
  DEPENDS binaries VERBATIM)

if(BUILD_TESTING)
  # Cross products are tested with native tools; target executables cannot run here.
  add_test(NAME frontend COMMAND "${CMAKE_COMMAND}" -E env ${cargo_environment}
    "${PRODUCT_CARGO}" test --locked)
  set_tests_properties(frontend PROPERTIES WORKING_DIRECTORY "${CMAKE_SOURCE_DIR}")
  add_test(NAME instigator COMMAND "${CMAKE_COMMAND}" -E env ${go_environment}
    "${PRODUCT_GO}" test -mod=readonly ./internal/qemunet ./cmd/instigator)
  set_tests_properties(instigator PROPERTIES WORKING_DIRECTORY "${CMAKE_SOURCE_DIR}/instigator")
endif()

install(PROGRAMS "${frontend_binary}" "${instigator_binary}" DESTINATION bin)
install(PROGRAMS "${qemu_build}/qemu-system-mips64${exe}" "${qemu_build}/qemu-img${exe}"
  DESTINATION libexec/origami)
install(DIRECTORY qemu/pc-bios/keymaps/ DESTINATION share/origami/qemu/keymaps
  PATTERN meson.build EXCLUDE)
install(FILES LICENSE DESTINATION share/origami/licenses RENAME origami.LICENSE)
install(FILES THIRD_PARTY_NOTICES DESTINATION share/origami/licenses)
install(FILES instigator/LICENSE DESTINATION share/origami/licenses RENAME instigator.LICENSE)
foreach(name LICENSE COPYING COPYING.LIB LICENSE.origami LICENSE.origami.paths)
  install(FILES "qemu/${name}" DESTINATION share/origami/licenses RENAME "qemu.${name}")
endforeach()
install(FILES qemu/hw/mips/sgi/models/LICENSE DESTINATION share/origami/licenses
  RENAME qemu.sgi-models.LICENSE)
# vcpkg installs a copyright file for each library it built.
file(GLOB vcpkg_copyrights "${vcpkg_prefix}/share/*/copyright")
foreach(copyright IN LISTS vcpkg_copyrights)
  get_filename_component(port "${copyright}" DIRECTORY)
  get_filename_component(port "${port}" NAME)
  install(FILES "${copyright}" DESTINATION share/origami/licenses RENAME "${port}.copyright")
endforeach()
if(PRODUCT_PLATFORM STREQUAL "windows")
  # MinGW's thread library comes from the toolchain only as a DLL.
  install(FILES /usr/x86_64-w64-mingw32/sys-root/mingw/bin/libwinpthread-1.dll
    DESTINATION libexec/origami)
endif()
install(CODE "set(platform ${PRODUCT_PLATFORM})
set(exe \"${exe}\")
set(cross_prefix \"${PRODUCT_CROSS_PREFIX}\")")
install(SCRIPT cmake/check-libraries.cmake)

if(PRODUCT_PLATFORM STREQUAL "macos")
  set(CPACK_PACKAGE_FILE_NAME origami-macos-arm64-preview)
  set(CPACK_PACKAGING_INSTALL_PREFIX /macos-arm64-dev)
else()
  set(CPACK_PACKAGE_FILE_NAME "origami-${PRODUCT_PLATFORM}-x86_64-preview")
  set(CPACK_PACKAGING_INSTALL_PREFIX "/${PRODUCT_PLATFORM}-dev")
endif()
set(CPACK_INCLUDE_TOPLEVEL_DIRECTORY OFF)
set(CPACK_PACKAGE_CHECKSUM SHA256)
if(PRODUCT_PLATFORM STREQUAL "windows")
  set(CPACK_GENERATOR ZIP)
else()
  set(CPACK_GENERATOR TGZ)
endif()
include(CPack)
