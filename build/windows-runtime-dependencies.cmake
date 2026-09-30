if(NOT DEFINED ROOTS OR NOT DEFINED SEARCH_DIRECTORIES OR NOT DEFINED SYSTEM_DLLS
   OR NOT DEFINED OUTPUT)
  message(FATAL_ERROR "ROOTS, SEARCH_DIRECTORIES, SYSTEM_DLLS, and OUTPUT are required")
endif()

set(CMAKE_GET_RUNTIME_DEPENDENCIES_PLATFORM windows+pe)
set(CMAKE_GET_RUNTIME_DEPENDENCIES_TOOL objdump)
set(CMAKE_GET_RUNTIME_DEPENDENCIES_COMMAND x86_64-w64-mingw32-objdump)

set(_pre_excludes)
foreach(_dll IN LISTS SYSTEM_DLLS)
  string(REPLACE "." "\\." _escaped_dll "${_dll}")
  list(APPEND _pre_excludes "^${_escaped_dll}$")
endforeach()
list(APPEND _pre_excludes "^(api-ms-win-|ext-ms-win-)")

file(GET_RUNTIME_DEPENDENCIES
  RESOLVED_DEPENDENCIES_VAR _resolved
  UNRESOLVED_DEPENDENCIES_VAR _unresolved
  CONFLICTING_DEPENDENCIES_PREFIX _conflicts
  EXECUTABLES ${ROOTS}
  DIRECTORIES ${SEARCH_DIRECTORIES}
  PRE_EXCLUDE_REGEXES ${_pre_excludes}
)

if(_unresolved)
  message(FATAL_ERROR "unresolved Windows DLL dependencies: ${_unresolved}")
endif()
if(_conflicts_FILENAMES)
  message(FATAL_ERROR "conflicting Windows DLL dependencies: ${_conflicts_FILENAMES}")
endif()

set(_runtime_libraries)
foreach(_dependency IN LISTS _resolved)
  get_filename_component(_name "${_dependency}" NAME)
  string(TOLOWER "${_name}" _lower_name)
  if(_lower_name STREQUAL "sdl2.dll")
    find_file(_sdl3_dll sdl3.dll PATHS ${SEARCH_DIRECTORIES} NO_DEFAULT_PATH)
    if(NOT _sdl3_dll)
      message(FATAL_ERROR "SDL2.dll is present but its SDL3.dll runtime dependency is missing")
    endif()
    list(APPEND _runtime_libraries "${_sdl3_dll}")
  endif()
endforeach()

if(_runtime_libraries)
  file(GET_RUNTIME_DEPENDENCIES
    RESOLVED_DEPENDENCIES_VAR _runtime_resolved
    UNRESOLVED_DEPENDENCIES_VAR _runtime_unresolved
    CONFLICTING_DEPENDENCIES_PREFIX _runtime_conflicts
    EXECUTABLES ${ROOTS}
    LIBRARIES ${_runtime_libraries}
    DIRECTORIES ${SEARCH_DIRECTORIES}
    PRE_EXCLUDE_REGEXES ${_pre_excludes}
  )
  if(_runtime_unresolved)
    message(FATAL_ERROR "unresolved Windows DLL dependencies: ${_runtime_unresolved}")
  endif()
  if(_runtime_conflicts_FILENAMES)
    message(FATAL_ERROR "conflicting Windows DLL dependencies: ${_runtime_conflicts_FILENAMES}")
  endif()
  list(APPEND _resolved ${_runtime_resolved} ${_runtime_libraries})
endif()

list(REMOVE_DUPLICATES _resolved)
list(SORT _resolved)
file(WRITE "${OUTPUT}" "")
foreach(_dependency IN LISTS _resolved)
  file(APPEND "${OUTPUT}" "${_dependency}\n")
endforeach()
