set(VCPKG_TARGET_ARCHITECTURE arm64)
set(VCPKG_CMAKE_SYSTEM_NAME Linux)
set(VCPKG_LIBRARY_LINKAGE static)
set(VCPKG_CRT_LINKAGE dynamic)

if(PORT STREQUAL "x265")
    # LLVM 10 supports NEON and dot-product; I8MM requires a newer compiler.
    list(APPEND VCPKG_CMAKE_CONFIGURE_OPTIONS "-DENABLE_NEON_I8MM=OFF")
endif()
