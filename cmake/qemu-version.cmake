# QEMU reports its package version in --version. Label it with the fork and
# the checkout's current description, refreshing it whenever that changes.
execute_process(
  COMMAND "${GIT_EXECUTABLE}" --no-optional-locks -C "${SOURCE}" describe --match "v*" --always --dirty
  OUTPUT_VARIABLE description OUTPUT_STRIP_TRAILING_WHITESPACE COMMAND_ERROR_IS_FATAL ANY)
set(label "sgi-origami ${description}")
file(READ "${BUILD}/meson-info/intro-buildoptions.json" options)
string(REGEX MATCH "\"name\": \"pkgversion\",[ \n]*\"value\": \"([^\"]*)\"" match "${options}")
if(NOT CMAKE_MATCH_1 STREQUAL label)
  execute_process(COMMAND "${BUILD}/pyvenv/bin/meson" configure "-Dpkgversion=${label}" "${BUILD}"
    OUTPUT_QUIET COMMAND_ERROR_IS_FATAL ANY)
endif()
