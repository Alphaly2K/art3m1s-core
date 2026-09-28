cmake_minimum_required(VERSION 3.28)

if(APPLE)
    project(art3m1s_krkr_host LANGUAGES C CXX OBJC OBJCXX)
else()
    project(art3m1s_krkr_host LANGUAGES C CXX)
endif()

option(
    ART3M1S_KRKR_VALIDATE_SOURCE_LISTS
    "Validate the generated upstream source lists without finding dependencies"
    OFF
)

if(NOT KRKRSDL3_SOURCE_DIR)
    message(FATAL_ERROR "KRKRSDL3_SOURCE_DIR is required")
endif()
if(NOT KRKRSDL3_BUILD_DIR)
    message(FATAL_ERROR "KRKRSDL3_BUILD_DIR is required")
endif()
if(ANDROID)
    set(_krkr_platform ANDROID)
elseif(CMAKE_SYSTEM_NAME STREQUAL "iOS")
    set(_krkr_platform IOS)
elseif(APPLE)
    set(_krkr_platform MACOS)
elseif(WIN32)
    set(_krkr_platform WINDOWS)
elseif(CMAKE_SYSTEM_NAME STREQUAL "Linux")
    set(_krkr_platform LINUX)
else()
    message(FATAL_ERROR "Unsupported KRKR host platform: ${CMAKE_SYSTEM_NAME}")
endif()
set(${_krkr_platform} TRUE)

set(CMAKE_C_STANDARD 17)
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

set(USE_SDL3 ON)
# Keep upstream's FFmpeg wave decoder when a bundled FFmpeg prefix is present.
# Several games label MP4/AAC streams as .ogg, which Vorbis cannot decode.
# The embedded host only needs wuffmpeg.cpp, not upstream's video/window code.
if(LINUX OR ANDROID OR (APPLE AND ART3M1S_FFMPEG_DIR))
    set(USE_FFMPEG ON)
else()
    set(USE_FFMPEG OFF)
endif()
set(USE_RENDER_GL OFF)
set(USE_RENDER_VK OFF)

set(_generated_dir "${CMAKE_CURRENT_BINARY_DIR}/generated")
file(MAKE_DIRECTORY "${_generated_dir}")

foreach(_list core environ plugins tjs2)
    set(_input "${KRKRSDL3_BUILD_DIR}/cmake/cmake_${_list}.txt")
    if(NOT EXISTS "${_input}")
        message(FATAL_ERROR "Missing upstream source list: ${_input}")
    endif()
    file(READ "${_input}" _contents)
    string(
        REPLACE
        "\${CMAKE_CURRENT_SOURCE_DIR}/cpp"
        "\${KRKRSDL3_SOURCE_DIR}"
        _contents
        "${_contents}"
    )
    file(WRITE "${_generated_dir}/cmake_${_list}.txt" "${_contents}")
endforeach()

include("${_generated_dir}/cmake_core.txt")
include("${_generated_dir}/cmake_environ.txt")
include("${_generated_dir}/cmake_plugins.txt")
include("${_generated_dir}/cmake_tjs2.txt")

if(ANDROID)
    # The upstream Android core assumes SDLActivity and its Java class loader.
    # Our Flutter embed supplies equivalent JNI/UI callbacks itself.
    list(REMOVE_ITEM KRKRSDL_ENVIRON_SRC
        "${KRKRSDL3_SOURCE_DIR}/environ/android/android_core.cpp")
endif()

list(
    FILTER
    KRKRSDL_ENVIRON_SRC
    EXCLUDE
    REGEX "/sdl3_entry\\.cpp$"
)
list(
    FILTER
    KRKRSDL_ENVIRON_SRC
    EXCLUDE
    REGEX "/sdl3_audio\\.cpp$"
)
list(
    FILTER
    KRKRSDL_ENVIRON_SRC
    EXCLUDE
    REGEX "/sdl3_app\\.cpp$"
)
list(
    FILTER
    KRKRSDL_ENVIRON_SRC
    EXCLUDE
    REGEX "/sdl3_input\\.cpp$"
)

set(_art3m1s_sources
    "${CMAKE_CURRENT_SOURCE_DIR}/capture_backend.cpp"
    "${CMAKE_CURRENT_SOURCE_DIR}/headless_platform.cpp"
    "${CMAKE_CURRENT_SOURCE_DIR}/host_audio.cpp"
    "${CMAKE_CURRENT_SOURCE_DIR}/host_runtime.cpp"
)
if(ANDROID)
    list(APPEND _art3m1s_sources "${CMAKE_CURRENT_SOURCE_DIR}/host_android.cpp")
endif()

set(_all_sources
    ${KRKRSDL_CORE_SRC}
    ${KRKRSDL_ENVIRON_SRC}
    ${KRKRSDL_PLUGINS_SRC}
    ${KRKRSDL_TJS2_SRC}
    ${_art3m1s_sources}
)

if(ART3M1S_KRKR_VALIDATE_SOURCE_LISTS)
    list(LENGTH _all_sources _source_count)
    message(STATUS "Validated ${_source_count} KRKR sources")
    return()
endif()

add_library(art3m1s_krkr_host SHARED ${_all_sources})

target_compile_definitions(
    art3m1s_krkr_host
    PRIVATE
        _KRKRSDL3_${_krkr_platform}=1
        _KRKRSDL3_USE_SDL3=1
        ART3M1S_KRKR_RESOURCE_EXE="${CMAKE_CURRENT_BINARY_DIR}/art3m1s-krkr"
)

target_include_directories(
    art3m1s_krkr_host
    PRIVATE
        "${CMAKE_CURRENT_SOURCE_DIR}"
        "${CMAKE_CURRENT_SOURCE_DIR}/../include"
        "${KRKRSDL3_SOURCE_DIR}/core/archive"
        "${KRKRSDL3_SOURCE_DIR}/core/main"
        "${KRKRSDL3_SOURCE_DIR}/core/media/font"
        "${KRKRSDL3_SOURCE_DIR}/core/media/image"
        "${KRKRSDL3_SOURCE_DIR}/core/media/movie"
        "${KRKRSDL3_SOURCE_DIR}/core/media/sound"
        "${KRKRSDL3_SOURCE_DIR}/core/msg"
        "${KRKRSDL3_SOURCE_DIR}/core/render"
        "${KRKRSDL3_SOURCE_DIR}/core/script"
        "${KRKRSDL3_SOURCE_DIR}/core/utils"
        "${KRKRSDL3_SOURCE_DIR}/core/utils/math"
        "${KRKRSDL3_SOURCE_DIR}/environ"
        "${KRKRSDL3_SOURCE_DIR}/plugins"
        "${KRKRSDL3_SOURCE_DIR}/tjs2"
)

if(APPLE AND ART3M1S_FFMPEG_DIR)
    foreach(_av_library avformat avcodec avutil)
        if(NOT EXISTS "${ART3M1S_FFMPEG_DIR}/lib/lib${_av_library}.dylib")
            message(FATAL_ERROR "Missing ${_av_library} in ${ART3M1S_FFMPEG_DIR}")
        endif()
    endforeach()
    target_include_directories(art3m1s_krkr_host PRIVATE
        "${ART3M1S_FFMPEG_DIR}/include")
    target_link_libraries(art3m1s_krkr_host PRIVATE
        "${ART3M1S_FFMPEG_DIR}/lib/libavformat.dylib"
        "${ART3M1S_FFMPEG_DIR}/lib/libavcodec.dylib"
        "${ART3M1S_FFMPEG_DIR}/lib/libavutil.dylib")
endif()

find_package(PNG REQUIRED)
find_package(Freetype REQUIRED)
find_package(glm CONFIG REQUIRED)
find_package(JPEG REQUIRED)
find_package(libjpeg-turbo CONFIG REQUIRED)
find_package(Vorbis CONFIG REQUIRED)
find_package(WebP CONFIG REQUIRED)
find_package(oniguruma CONFIG REQUIRED)
find_package(Opus CONFIG REQUIRED)
find_package(OpusFile CONFIG REQUIRED)
find_package(SDL3 CONFIG REQUIRED)
find_package(SDL3_ttf CONFIG REQUIRED)
find_package(plutovg CONFIG REQUIRED)

target_link_libraries(
    art3m1s_krkr_host
    PRIVATE
        PNG::PNG
        Freetype::Freetype
        glm::glm-header-only
        JPEG::JPEG
        $<IF:$<TARGET_EXISTS:libjpeg-turbo::turbojpeg>,libjpeg-turbo::turbojpeg,libjpeg-turbo::turbojpeg-static>
        Vorbis::vorbis
        Vorbis::vorbisfile
        Vorbis::vorbisenc
        WebP::webp
        WebP::webpdecoder
        WebP::webpdemux
        oniguruma::onig
        Opus::opus
        OpusFile::opusfile
        SDL3::SDL3
        SDL3_ttf::SDL3_ttf
        plutovg::plutovg
)

if(MACOS)
    target_link_libraries(art3m1s_krkr_host PRIVATE
        "-framework Foundation" "-framework AppKit" "-framework CoreGraphics"
        "-framework QuartzCore" "-framework IOKit" "-framework ForceFeedback"
        "-framework GameController" "-framework AudioToolbox" "-framework Carbon"
        "-framework AVFoundation")
elseif(IOS)
    target_link_libraries(art3m1s_krkr_host PRIVATE
        "-framework Foundation" "-framework UIKit" "-framework CoreGraphics"
        "-framework QuartzCore" "-framework AVFoundation" "-framework AudioToolbox"
        "-framework GameController" "-framework Metal" "-framework OpenGLES")
elseif(ANDROID)
    target_link_libraries(art3m1s_krkr_host PRIVATE android mediandk EGL GLESv3)
elseif(WINDOWS)
    target_link_libraries(art3m1s_krkr_host PRIVATE mfplat mfuuid mfreadwrite)
endif()

set_target_properties(
    art3m1s_krkr_host
    PROPERTIES
        CXX_VISIBILITY_PRESET hidden
        VISIBILITY_INLINES_HIDDEN YES
)
if(APPLE)
    set_target_properties(art3m1s_krkr_host PROPERTIES
        BUILD_RPATH "@loader_path" INSTALL_RPATH "@loader_path"
        INSTALL_NAME_DIR "@rpath" MACOSX_RPATH YES)
elseif(UNIX)
    set_target_properties(art3m1s_krkr_host PROPERTIES
        BUILD_RPATH "$ORIGIN" INSTALL_RPATH "$ORIGIN")
endif()

add_custom_command(
    TARGET art3m1s_krkr_host
    POST_BUILD
    COMMAND
        ${CMAKE_COMMAND} -E copy_directory
        "${KRKRSDL3_BUILD_DIR}/Res"
        "${CMAKE_CURRENT_BINARY_DIR}/Res"
)
