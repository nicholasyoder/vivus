cmake_minimum_required(VERSION 3.21)
project(vivus-sidebar VERSION 0.1.0 LANGUAGES CXX)

find_package(Qt6 6.8 REQUIRED COMPONENTS Widgets DBus)
find_package(LayerShellQt REQUIRED)

qt_standard_project_setup(REQUIRES 6.8)

set(DBUS_SRCS)
qt_add_dbus_interface(DBUS_SRCS ${CMAKE_CURRENT_SOURCE_DIR}/../dbus/org.vivus.Assistant.xml vivus_interface)

qt_add_executable(vivus-sidebar
    src/main.cpp
    src/daemonclient.h src/daemonclient.cpp
    src/sidebarwindow.h src/sidebarwindow.cpp
    ${DBUS_SRCS}
)

qt_add_resources(vivus-sidebar style
    PREFIX /vivus
    BASE data
    FILES data/sidebar.qss
)

target_include_directories(vivus-sidebar PRIVATE src ${CMAKE_CURRENT_BINARY_DIR})
target_link_libraries(vivus-sidebar PRIVATE
    Qt6::Widgets
    Qt6::DBus
    LayerShellQt::Interface
)

include(GNUInstallDirs)
install(TARGETS vivus-sidebar RUNTIME DESTINATION ${CMAKE_INSTALL_BINDIR})
install(FILES data/org.vivus.Vivus.desktop DESTINATION ${CMAKE_INSTALL_DATADIR}/applications)
