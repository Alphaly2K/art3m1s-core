cmake_minimum_required(VERSION 3.20)

project(art3m1s_krkr_host LANGUAGES C CXX)

set(CMAKE_C_STANDARD 11)
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

add_library(art3m1s_krkr_host SHARED
    src/bootstrap.cpp
)

target_include_directories(art3m1s_krkr_host
    PUBLIC
        ${CMAKE_CURRENT_SOURCE_DIR}/include
)

set_target_properties(art3m1s_krkr_host PROPERTIES
    CXX_VISIBILITY_PRESET hidden
    VISIBILITY_INLINES_HIDDEN YES
    BUILD_RPATH "@loader_path"
    INSTALL_RPATH "@loader_path"
    MACOSX_RPATH YES
)

if(APPLE)
    set_target_properties(art3m1s_krkr_host PROPERTIES
        INSTALL_NAME_DIR "@rpath"
    )
endif()
