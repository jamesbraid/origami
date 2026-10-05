# Fail the install when an executable needs a shared library that is neither
# in the archive beside it nor part of the host platform. Run by install();
# the caller sets platform, exe and cross_prefix.
set(root "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}")
if(platform STREQUAL "linux")
  # Graphics, display, audio and device libraries must come from the host to
  # match its drivers and services.
  set(system "^(ld-linux-x86-64|lib(c|m|dl|rt|pthread|gcc_s|X11|X11-xcb|xcb|xkbcommon|wayland-[a-z]+|\
udev|asound|pulse))\\.so")
elseif(platform STREQUAL "macos")
  set(system "^/usr/lib/" "^/System/Library/")
else()
  set(CMAKE_GET_RUNTIME_DEPENDENCIES_PLATFORM windows+pe)
  set(CMAKE_GET_RUNTIME_DEPENDENCIES_TOOL objdump)
  set(CMAKE_GET_RUNTIME_DEPENDENCIES_COMMAND "${cross_prefix}objdump")
  set(system advapi32 bcrypt bcryptprimitives cfgmgr32 combase comctl32 comdlg32 crypt32
    d3d11 d3d9 dnsapi dwmapi dxgi gdi32 imm32 iphlpapi kernel32 msvcrt ntdll ole32 oleaut32
    setupapi shell32 shlwapi user32 userenv uxtheme version winmm ws2_32)
  list(TRANSFORM system PREPEND "^")
  list(TRANSFORM system APPEND "\\.dll$")
  list(APPEND system "^(api-ms-win-|ext-ms-win-)")
endif()
foreach(directory IN ITEMS bin libexec/origami)
  file(GLOB executables "${root}/${directory}/*${exe}")
  file(GET_RUNTIME_DEPENDENCIES EXECUTABLES ${executables}
    DIRECTORIES "${root}/${directory}"
    RESOLVED_DEPENDENCIES_VAR libraries
    UNRESOLVED_DEPENDENCIES_VAR missing
    PRE_EXCLUDE_REGEXES ${system})
  foreach(library IN LISTS libraries)
    get_filename_component(location "${library}" DIRECTORY)
    if(NOT location STREQUAL "${root}/${directory}")
      list(APPEND missing "${library}")
    endif()
  endforeach()
  if(missing)
    message(FATAL_ERROR "${directory} needs libraries outside the archive: ${missing}")
  endif()
endforeach()
