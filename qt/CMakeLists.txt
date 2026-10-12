# Slerp's Qt image plugin, and the program that tests it.
#
#   cmake -S qt -B local/target-qt -DCMAKE_BUILD_TYPE=Release
#   cmake --build local/target-qt -j 4
#
# The plugin lands in local/target-qt/plugins/imageformats/, so a test can
# point QT_PLUGIN_PATH at local/target-qt/plugins.  To install it for every
# Qt program, copy slerp_qimage.so into Qt's own plugin directory
# (qtpaths6 --query QT_INSTALL_PLUGINS, then imageformats/); Qt doesn't
# search /usr/local.  setup.yml does that.
cmake_minimum_required(VERSION 3.20)
project(slerp_qimage LANGUAGES CXX)
set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_AUTOMOC ON)
if(NOT CMAKE_BUILD_TYPE)
	set(CMAKE_BUILD_TYPE Release)
endif()
find_package(Qt6 REQUIRED COMPONENTS Core Gui)

# Where the plugin starts the helper from when none is running.
set(SLERP_EXECUTABLE "/usr/local/bin/slerp" CACHE STRING "The slerp program the plugin starts its helper from")

add_library(slerp_qimage MODULE plugin.cpp client.cpp formats.cpp choices.cpp)
# moc embeds slerp.json in the plugin; rebuild when it changes.
set_property(SOURCE plugin.cpp APPEND PROPERTY OBJECT_DEPENDS "${CMAKE_CURRENT_SOURCE_DIR}/slerp.json")
set_property(TARGET slerp_qimage APPEND PROPERTY AUTOMOC_DEPEND_FILTERS "Q_PLUGIN_METADATA" "FILE \"([^\"]+)\"")
target_link_libraries(slerp_qimage PRIVATE Qt6::Core Qt6::Gui)
target_compile_definitions(slerp_qimage PRIVATE SLERP_EXECUTABLE="${SLERP_EXECUTABLE}" QT_NO_KEYWORDS)
target_compile_options(slerp_qimage PRIVATE -Wall -Wextra)
set_target_properties(slerp_qimage PROPERTIES
	PREFIX ""
	CXX_VISIBILITY_PRESET hidden
	VISIBILITY_INLINES_HIDDEN ON
	LIBRARY_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/plugins/imageformats")
# Missing symbols fail the build rather than a host's dlopen.  nodelete
# keeps the plugin mapped for the life of the process, so a turntable still
# being fetched on its own thread when the host exits can't run unmapped code.
target_link_options(slerp_qimage PRIVATE -Wl,--no-undefined -Wl,-z,relro,-z,now,-z,nodelete)

# The test program: reads files through QImageReader and QMovie as Dolphin
# and Gwenview do, times ordinary images with and without the plugin, and
# checks the format table, the content checks and the protocol client.
add_executable(slerp-qt-check tests/check.cpp client.cpp formats.cpp choices.cpp)
target_link_libraries(slerp-qt-check PRIVATE Qt6::Core Qt6::Gui)
target_compile_definitions(slerp-qt-check PRIVATE SLERP_EXECUTABLE="${SLERP_EXECUTABLE}"
	SLERP_PLUGIN_JSON="${CMAKE_CURRENT_SOURCE_DIR}/slerp.json"
	SLERP_MIME_XML="${CMAKE_CURRENT_SOURCE_DIR}/../data/slerp.xml")
target_include_directories(slerp-qt-check PRIVATE "${CMAKE_CURRENT_SOURCE_DIR}")
target_compile_options(slerp-qt-check PRIVATE -Wall -Wextra)

install(TARGETS slerp_qimage LIBRARY DESTINATION "${QT6_INSTALL_PLUGINS}/imageformats")
