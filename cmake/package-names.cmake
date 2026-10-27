# Runs inside cpack. Names the archive origami-<version>-<platform> and its
# top-level directory origami-<version>. The version is the nearest v* tag
# without the "v": 0.4.1 at a release, 0.4.0-5-gabc1234 after one.
execute_process(
  COMMAND "${CPACK_ORIGAMI_GIT}" --no-optional-locks -C "${CPACK_ORIGAMI_SOURCE_DIR}"
    describe --match "v[0-9]*" --always --dirty
  OUTPUT_VARIABLE version OUTPUT_STRIP_TRAILING_WHITESPACE RESULT_VARIABLE status
  ERROR_QUIET)
if(NOT status EQUAL 0 OR version STREQUAL "")
  set(version unknown)
endif()
string(REGEX REPLACE "^v" "" version "${version}")
set(CPACK_PACKAGE_FILE_NAME "origami-${version}-${CPACK_ORIGAMI_PLATFORM}")
set(CPACK_PACKAGING_INSTALL_PREFIX "/origami-${version}")
