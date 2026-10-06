# Cross-compile with llvm-mingw (Wine tests): Windows x86_64.
set(CMAKE_SYSTEM_NAME Windows)
set(CMAKE_SYSTEM_PROCESSOR x86_64)
set(CMAKE_C_COMPILER x86_64-w64-mingw32-clang)
set(CMAKE_CXX_COMPILER x86_64-w64-mingw32-clang++)
set(CMAKE_RC_COMPILER x86_64-w64-mingw32-windres)
set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
