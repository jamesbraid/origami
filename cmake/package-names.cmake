# Runs inside cpack. Names the archive origami-<version>-<platform> and its
# top-level directory origami-<version>. The version is the nearest v* tag
# without the "v": 0.4.1 at a release, 0.4.0-5-gabc1234 after one.
execute_process(
  COMMAND "${CPACK_ORIGAMI_GIT}" --no-optional-locks -C "${CPACK_ORIGAMI_SOURCE_DIR}"
    describe --match "v[0-9]*" --always
  OUTPUT_VARIABLE version OUTPUT_STRIP_TRAILING_WHITESPACE RESULT_VARIABLE status
  ERROR_QUIET)
if(NOT status EQUAL 0 OR version STREQUAL "")
  set(version unknown)
endif()
# The product build rewrites files inside the QEMU submodule, so only edits
# to the superproject's own tracked files make the version dirty.
# Refresh first: a touched but unchanged file would otherwise look modified.
execute_process(
  COMMAND "${CPACK_ORIGAMI_GIT}" -C "${CPACK_ORIGAMI_SOURCE_DIR}"
    update-index -q --ignore-submodules --refresh
  OUTPUT_QUIET ERROR_QUIET)
execute_process(
  COMMAND "${CPACK_ORIGAMI_GIT}" --no-optional-locks -C "${CPACK_ORIGAMI_SOURCE_DIR}"
    diff-index --quiet --ignore-submodules=dirty HEAD --
  RESULT_VARIABLE changed ERROR_QUIET)
if(NOT version STREQUAL "unknown" AND NOT changed EQUAL 0)
  string(APPEND version "-dirty")
endif()
string(REGEX REPLACE "^v" "" version "${version}")
set(CPACK_PACKAGE_FILE_NAME "origami-${version}-${CPACK_ORIGAMI_PLATFORM}")
set(CPACK_PACKAGING_INSTALL_PREFIX "/origami-${version}")
