# Copy the shared libraries the installed executables need into the install
# tree. Run by install(); the caller sets platform, exe, cross_prefix and scratch.
set(root "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}")
set(qemu "${root}/libexec/origami/qemu-system-mips64${exe}" "${root}/libexec/origami/qemu-img${exe}")
# Each bundled library needs an entry in THIRD_PARTY_NOTICES before it ships.
set(noticed "^(lib(glib|gio|gobject|gmodule)-2\\.0|libintl|(lib)?iconv|libpcre2|libffi|libpixman-1|\
(lib)?sdl[23]|libepoxy|libdecor-0|libsamplerate|libxss|libatomic|\
libgcc_s|libwinpthread|zlib1|libpng16|libjpeg|libtiff|libwebp|libsharpyuv|libnettle|libhogweed|libgmp)")

function(check_noticed files)
  foreach(file IN LISTS files)
    get_filename_component(name "${file}" NAME)
    string(TOLOWER "${name}" name)
    if(NOT name MATCHES "${noticed}")
      message(FATAL_ERROR "${name} would ship without a notice; add it to THIRD_PARTY_NOTICES and cmake/bundle.cmake")
    endif()
  endforeach()
endfunction()

if(platform STREQUAL "linux")
  # Bundle what QEMU needs beyond a desktop's own graphics, audio, display and
  # system services. Those must come from the host to match its drivers.
  # Clear the previous bundle first so a reinstall resolves from the system.
  set(libdir "${root}/lib/origami")
  file(REMOVE_RECURSE "${libdir}")
  file(GET_RUNTIME_DEPENDENCIES EXECUTABLES ${qemu}
    RESOLVED_DEPENDENCIES_VAR libraries
    PRE_INCLUDE_REGEXES "^lib(glib|gio|gobject|gmodule)-2\\.0\\.so" "^libpixman-1\\.so"
      "^libSDL2-2\\.0\\.so" "^libepoxy\\.so" "^libdecor-0\\.so" "^libsamplerate\\.so"
      "^libXss\\.so" "^libatomic\\.so"
    PRE_EXCLUDE_REGEXES ".*")
  check_noticed("${libraries}")
  file(MAKE_DIRECTORY "${libdir}")
  foreach(library IN LISTS libraries)
    get_filename_component(name "${library}" NAME)
    file(REAL_PATH "${library}" source)
    file(COPY_FILE "${source}" "${libdir}/${name}")
    # CMake can only replace an existing RPATH, and these have none.
    execute_process(COMMAND patchelf --set-rpath "\$ORIGIN" "${libdir}/${name}"
      COMMAND_ERROR_IS_FATAL ANY)
  endforeach()
  foreach(binary IN LISTS qemu)
    execute_process(COMMAND patchelf --set-rpath "\$ORIGIN/../../lib/origami" "${binary}"
      COMMAND_ERROR_IS_FATAL ANY)
  endforeach()
elseif(platform STREQUAL "windows")
  # Windows has no shared library stack to rely on: bundle everything outside
  # the operating system next to the executables that load it.
  set(CMAKE_GET_RUNTIME_DEPENDENCIES_PLATFORM windows+pe)
  set(CMAKE_GET_RUNTIME_DEPENDENCIES_TOOL objdump)
  set(CMAKE_GET_RUNTIME_DEPENDENCIES_COMMAND "${cross_prefix}objdump")
  set(sysroot /usr/x86_64-w64-mingw32/sys-root/mingw/bin)
  # CMake looks up DLLs by lowercased name, which misses SDL2.dll on a
  # case-sensitive host (CMake issue 28049). Remove these aliases once the
  # build images have a CMake release containing its fix.
  set(aliases "${scratch}/dlls")
  file(REMOVE_RECURSE "${aliases}")
  file(MAKE_DIRECTORY "${aliases}")
  file(GLOB dlls "${sysroot}/*.dll")
  foreach(dll IN LISTS dlls)
    get_filename_component(name "${dll}" NAME)
    string(TOLOWER "${name}" lower)
    foreach(alias IN ITEMS "${name}" "${lower}")
      if(NOT EXISTS "${aliases}/${alias}")
        file(CREATE_LINK "${dll}" "${aliases}/${alias}" SYMBOLIC)
      endif()
    endforeach()
  endforeach()
  set(system advapi32 bcrypt bcryptprimitives cfgmgr32 combase comctl32 comdlg32 crypt32
    d3d11 d3d9 dnsapi dwmapi dxgi gdi32 imm32 iphlpapi kernel32 msvcrt ntdll ole32 oleaut32
    setupapi shell32 shlwapi user32 userenv uxtheme version winmm ws2_32)
  list(TRANSFORM system PREPEND "^")
  list(TRANSFORM system APPEND "\\.dll$")
  foreach(directory IN ITEMS bin libexec/origami)
    file(GLOB executables "${root}/${directory}/*.exe")
    file(GLOB stale "${root}/${directory}/*.dll")
    file(REMOVE ${stale})
    # SDL2 here is sdl2-compat, which loads SDL3 at run time.
    set(loaded "")
    if(directory STREQUAL "libexec/origami" AND EXISTS "${aliases}/SDL3.dll")
      set(loaded "${aliases}/SDL3.dll")
    endif()
    file(GET_RUNTIME_DEPENDENCIES EXECUTABLES ${executables} LIBRARIES ${loaded}
      DIRECTORIES "${aliases}"
      RESOLVED_DEPENDENCIES_VAR libraries
      UNRESOLVED_DEPENDENCIES_VAR missing
      CONFLICTING_DEPENDENCIES_PREFIX conflicts
      PRE_EXCLUDE_REGEXES ${system} "^(api-ms-win-|ext-ms-win-)")
    if(missing OR conflicts_FILENAMES)
      message(FATAL_ERROR "Unresolved Windows DLLs: ${missing} ${conflicts_FILENAMES}")
    endif()
    list(APPEND libraries ${loaded})
    check_noticed("${libraries}")
    foreach(library IN LISTS libraries)
      file(REAL_PATH "${library}" source)
      get_filename_component(name "${source}" NAME)
      file(COPY_FILE "${source}" "${root}/${directory}/${name}")
    endforeach()
  endforeach()
else()
  # dylibbundler copies the dylibs, rewrites load paths relative to the
  # executable and re-signs everything it changes.
  file(REMOVE_RECURSE "${root}/lib/origami")
  set(command dylibbundler -od -b -d "${root}/lib/origami" -p @executable_path/../../lib/origami/)
  foreach(binary IN LISTS qemu)
    list(APPEND command -x "${binary}")
  endforeach()
  execute_process(COMMAND ${command} COMMAND_ERROR_IS_FATAL ANY)
  file(GLOB libraries "${root}/lib/origami/*.dylib")
  check_noticed("${libraries}")
endif()
