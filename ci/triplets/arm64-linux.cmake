set(VCPKG_TARGET_ARCHITECTURE arm64)
set(VCPKG_CMAKE_SYSTEM_NAME Linux)
set(VCPKG_LIBRARY_LINKAGE static)
set(VCPKG_CRT_LINKAGE dynamic)

# LLVM 10's NEON dot-product header mixes signed/unsigned vectors of identical size.
set(VCPKG_C_FLAGS "-flax-vector-conversions")
set(VCPKG_CXX_FLAGS "-flax-vector-conversions")
