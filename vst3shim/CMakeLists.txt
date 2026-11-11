cmake_minimum_required(VERSION 3.25)
project(solstice_vst3 CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

set(SMTG_ENABLE_VST3_PLUGIN_EXAMPLES ${SOLSTICE_SDK_EXAMPLES} CACHE BOOL "" FORCE)
set(SMTG_ENABLE_VST3_HOSTING_EXAMPLES OFF CACHE BOOL "" FORCE)
set(SMTG_ENABLE_VSTGUI_SUPPORT OFF CACHE BOOL "" FORCE)
set(SMTG_RUN_VST_VALIDATOR OFF CACHE BOOL "" FORCE)
set(SMTG_CREATE_PLUGIN_LINK OFF CACHE BOOL "" FORCE)
add_subdirectory(vst3sdk EXCLUDE_FROM_ALL)

find_package(X11 REQUIRED)

add_library(solstice_vst3 STATIC
    shim.cpp
    x11window.cpp
    vst3sdk/public.sdk/source/vst/hosting/module_linux.cpp
)
target_link_libraries(solstice_vst3 PRIVATE sdk_hosting X11::X11)
install(TARGETS solstice_vst3 DESTINATION lib)
