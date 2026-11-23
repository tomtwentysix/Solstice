#!/bin/sh
# Builds the GTK 4 and libadwaita stack Solstice needs into /opt/solstice, on a Debian release
# whose own GTK is too old. Runs inside the container from packaging/deb/Containerfile.
#
# Every library carries a run path to /opt/solstice/lib rather than relying on
# LD_LIBRARY_PATH, which plug-in hosts started by Solstice (yabridge, Wine) would inherit.
set -e
prefix=/opt/solstice
export PATH="$prefix/bin:/opt/meson/bin:$PATH"
export PKG_CONFIG_PATH="$prefix/lib/pkgconfig:$prefix/share/pkgconfig"
export LDFLAGS="-Wl,-rpath,$prefix/lib"
mkdir -p /build && cd /build

# name version url [meson options...]
build() {
    name=$1 version=$2 url=$3
    shift 3
    dir="$name-$version"
    if [ ! -d "$dir" ]; then
        curl -sSfL -o "$dir.tar" "$url"
        # Whatever the archive's top folder is called (some start with ./), it becomes $dir.
        tmp=$(mktemp -d -p .)
        tar xf "$dir.tar" -C "$tmp"
        mv "$tmp"/* "$dir" && rmdir "$tmp"
    fi
    [ -f "$dir/_b/build.ninja" ] || /opt/meson/bin/meson setup "$dir/_b" "$dir" --prefix="$prefix" --libdir=lib \
        --buildtype=release --wrap-mode="${wrap:-nofallback}" "$@"
    ninja -C "$dir/_b" install
}

gnome=https://download.gnome.org/sources
fdo=https://gitlab.freedesktop.org

# Debian's own GIO modules (dconf settings, gvfs) load into the newer GLib.
build glib 2.84.4 "$gnome/glib/2.84/glib-2.84.4.tar.xz" \
    -Dgio_module_dir=/usr/lib/x86_64-linux-gnu/gio/modules -Dtests=false -Dintrospection=disabled -Dman-pages=disabled -Ddocumentation=false -Dsysprof=disabled -Dselinux=disabled
build harfbuzz 10.4.0 https://github.com/harfbuzz/harfbuzz/releases/download/10.4.0/harfbuzz-10.4.0.tar.xz \
    -Dtests=disabled -Ddocs=disabled -Dintrospection=disabled -Dcairo=disabled -Dglib=enabled -Dfreetype=enabled -Dutilities=disabled
# Pango needs a newer fontconfig; this one reads the system's /etc/fonts and shares its cache.
build fontconfig 2.16.0 https://www.freedesktop.org/software/fontconfig/release/fontconfig-2.16.0.tar.xz \
    --sysconfdir=/etc --localstatedir=/var -Ddoc=disabled -Dtests=disabled -Dtools=disabled -Dcache-build=disabled
build cairo 1.18.4 https://cairographics.org/releases/cairo-1.18.4.tar.xz \
    -Dtests=disabled -Dspectre=disabled -Dlzo=disabled
build pango 1.56.4 "$gnome/pango/1.56/pango-1.56.4.tar.xz" \
    -Dintrospection=disabled -Ddocumentation=false -Dbuild-testsuite=false -Dbuild-examples=false -Dlibthai=disabled
build graphene 1.10.8 "$gnome/graphene/1.10/graphene-1.10.8.tar.xz" \
    -Dtests=false -Dinstalled_tests=false -Dintrospection=disabled
build gdk-pixbuf 2.42.12 "$gnome/gdk-pixbuf/2.42/gdk-pixbuf-2.42.12.tar.xz" \
    -Dintrospection=disabled -Dman=false -Dtests=false -Dinstalled_tests=false -Dbuiltin_loaders=all -Dothers=disabled
build wayland 1.24.0 "$fdo/wayland/wayland/-/releases/1.24.0/downloads/wayland-1.24.0.tar.xz" \
    -Ddocumentation=false -Dtests=false -Ddtd_validation=false
build wayland-protocols 1.45 "$fdo/wayland/wayland-protocols/-/releases/1.45/downloads/wayland-protocols-1.45.tar.xz" \
    -Dtests=false
# GTK builds its own C-only librsvg branch (librsvg-gtk-2.40.23, with libcroco pinned to a
# commit) as a subproject, which spares the Rust librsvg and its toolchain.
wrap=default
build gtk 4.20.4 "$gnome/gtk/4.20/gtk-4.20.4.tar.xz" --force-fallback-for=librsvg,libcroco \
    -Dintrospection=disabled -Ddocumentation=false -Dman-pages=false -Dbuild-tests=false -Dbuild-testsuite=false \
    -Dbuild-examples=false -Dbuild-demos=false -Dmedia-gstreamer=disabled -Dprint-cups=disabled -Dvulkan=disabled \
    -Dcolord=disabled -Dtracker=disabled -Dcloudproviders=disabled -Dsysprof=disabled -Daccesskit=disabled
wrap=
build libxmlb 0.3.22 https://github.com/hughsie/libxmlb/releases/download/0.3.22/libxmlb-0.3.22.tar.xz \
    -Dintrospection=false -Dgtkdoc=false -Dtests=false -Dcli=false -Dstemmer=false
build AppStream 1.0.4 https://www.freedesktop.org/software/appstream/releases/AppStream-1.0.4.tar.xz \
    -Dstemming=false -Dsystemd=false -Dgir=false -Dapidocs=false -Dinstall-docs=false -Dvapi=false -Dqt=false -Dcompose=false
build libadwaita 1.8.8 "$gnome/libadwaita/1.8/libadwaita-1.8.8.tar.xz" \
    -Dintrospection=disabled -Dvapi=false -Ddocumentation=false -Dtests=false -Dexamples=false

glib-compile-schemas "$prefix/share/glib-2.0/schemas"
