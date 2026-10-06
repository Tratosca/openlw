# OpenLW Windows audio driver (in-process COM DLL, ASIO interface), GPLv3.
#   windows/driver/fetch-sdk.ps1 (or .sh)       ASIO SDK in sdk/
#   cmake -S windows/driver -B build/driver -A ARM64   (or x64), then cmake --build build/driver --config Release
# Cross-compilation (Wine tests): -DCMAKE_TOOLCHAIN_FILE=cmake/llvm-mingw-aarch64.cmake
cmake_minimum_required(VERSION 3.20)
project(OpenLWDriver LANGUAGES C CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_C_STANDARD 11)
# Static C/C++ runtime: host applications load the driver without the Visual C++ Redistributable.
set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>")

set(ASIOSDK "${CMAKE_CURRENT_SOURCE_DIR}/sdk/ASIOSDK" CACHE PATH "SDK ASIO de Steinberg")
if(NOT EXISTS "${ASIOSDK}/common/iasiodrv.h")
    message(FATAL_ERROR "SDK ASIO absent : lancer windows/driver/fetch-sdk.ps1 (ou .sh)")
endif()
set(LWSYS "${CMAKE_CURRENT_SOURCE_DIR}/../../daemon/lw-sys/csrc")

# OpenLW system layer/shared region (Apache-2.0), compiled into driver.
add_library(lwsys STATIC "${LWSYS}/lw_shm.c" "${LWSYS}/windows/lw_sys_win.c")
target_include_directories(lwsys PUBLIC "${LWSYS}")
target_compile_definitions(lwsys PRIVATE _CRT_SECURE_NO_WARNINGS)
target_link_libraries(lwsys PUBLIC avrt advapi32 qwave ws2_32)

add_library(OpenLWDriver SHARED src/driver.cpp src/OpenLWDriver.def)
target_include_directories(OpenLWDriver PRIVATE "${ASIOSDK}/common")
target_compile_definitions(OpenLWDriver PRIVATE UNICODE _UNICODE WIN32_LEAN_AND_MEAN NOMINMAX)
target_link_libraries(OpenLWDriver PRIVATE lwsys ole32 oleaut32 uuid winmm shell32 advapi32)
# File name: OpenLWDriver.dll, or OpenLWDriver_arm64/_x64.dll behind the ARM64X forwarder (arm64x/).
set(OPENLW_DRIVER_NAME "OpenLWDriver" CACHE STRING "Driver DLL name, without extension")
set_target_properties(OpenLWDriver PROPERTIES PREFIX "" OUTPUT_NAME "${OPENLW_DRIVER_NAME}")
if(MSVC)
    target_compile_options(OpenLWDriver PRIVATE /W4 /WX /utf-8)
    target_compile_options(lwsys PRIVATE /W3 /WX /utf-8)
else()
    target_compile_options(OpenLWDriver PRIVATE -Wall -Wextra -Werror)
    target_link_options(OpenLWDriver PRIVATE -static)
endif()

# Test host: load driver as an ASIO application and verify audio pipeline.
add_executable(openlw-driver-test test/host.cpp)
target_include_directories(openlw-driver-test PRIVATE "${ASIOSDK}/common")
target_compile_definitions(openlw-driver-test PRIVATE UNICODE _UNICODE WIN32_LEAN_AND_MEAN NOMINMAX)
target_link_libraries(openlw-driver-test PRIVATE ole32 uuid)
if(NOT MSVC)
    target_compile_options(openlw-driver-test PRIVATE -Wall -Wextra -Werror)
    target_link_options(openlw-driver-test PRIVATE -static -municode)
endif()
