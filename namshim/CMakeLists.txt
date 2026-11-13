cmake_minimum_required(VERSION 3.25)
project(solstice_nam CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

set(NAM ${CMAKE_CURRENT_SOURCE_DIR}/NeuralAmpModelerCore)
file(GLOB NAM_SOURCES ${NAM}/NAM/*.cpp ${NAM}/NAM/*/*.cpp)

# The submodule stays pristine: patched copies of the affected files are compiled instead.
set(PATCH ${CMAKE_CURRENT_SOURCE_DIR}/patches/convnet-no-alloc.patch)
set(STAGE ${CMAKE_CURRENT_BINARY_DIR}/stage)
set(PATCHED ${CMAKE_CURRENT_BINARY_DIR}/patched/NAM)
set_property(DIRECTORY APPEND PROPERTY CMAKE_CONFIGURE_DEPENDS ${PATCH})
file(REMOVE_RECURSE ${STAGE})
file(COPY ${NAM}/NAM/convnet.cpp ${NAM}/NAM/convnet.h DESTINATION ${STAGE}/NAM)
execute_process(COMMAND patch -p1 -s -d ${STAGE} -i ${PATCH} COMMAND_ERROR_IS_FATAL ANY)
file(MAKE_DIRECTORY ${PATCHED})
foreach(f convnet.cpp convnet.h)
    file(COPY_FILE ${STAGE}/NAM/${f} ${PATCHED}/${f} ONLY_IF_DIFFERENT)
endforeach()
list(REMOVE_ITEM NAM_SOURCES ${NAM}/NAM/convnet.cpp)
set_source_files_properties(${PATCHED}/convnet.cpp PROPERTIES INCLUDE_DIRECTORIES ${NAM}/NAM)

add_library(solstice_nam STATIC bridge.cpp ${NAM_SOURCES} ${PATCHED}/convnet.cpp)
target_include_directories(solstice_nam PRIVATE ${NAM})
target_include_directories(solstice_nam SYSTEM PRIVATE ${NAM}/Dependencies/eigen ${NAM}/Dependencies/nlohmann)
target_compile_definitions(solstice_nam PRIVATE NAM_SAMPLE_FLOAT NAM_ENABLE_A2_FAST)
install(TARGETS solstice_nam DESTINATION lib)
