#!/bin/sh
# Builds Solstice from the source tarball and packages it with the toolkit in /opt/solstice.
# Runs inside the container from packaging/deb/Containerfile.
# Usage: package.sh <tarball> <debian version> <output dir> <maintainer email>
set -e
tarball=$1 debversion=$2 out=$3 email=$4
prefix=/opt/solstice
triplet=x86_64-linux-gnu

work=$(mktemp -d)
# Always the same path: CMake's caches in the reused target directory record where the sources were.
src=/work/src
rm -rf "$src" && mkdir -p "$src"
tar xzf "$tarball" -C "$src" --strip-components=1
cd "$src"

export PKG_CONFIG_PATH="$prefix/lib/pkgconfig:$prefix/share/pkgconfig"
# DT_RPATH rather than RUNPATH, so a user's LD_LIBRARY_PATH can't pull Debian's older GTK in.
export RUSTFLAGS="-C link-arg=-Wl,-rpath,$prefix/lib -C link-arg=-Wl,--disable-new-dtags"
cargo build --release --locked

root=$work/root
mkdir -p "$root$prefix/bin" "$root$prefix/lib" "$root$prefix/share/glib-2.0" "$root/usr/bin"
install -m755 "${CARGO_TARGET_DIR:-target}/release/solstice" "${CARGO_TARGET_DIR:-target}/release/solstice-scan" "$root$prefix/bin/"
cp -a "$prefix"/lib/*.so.* "$root$prefix/lib/"
cp -a "$prefix/share/glib-2.0/schemas" "$root$prefix/share/glib-2.0/"
for domain in glib20 gtk40 libadwaita gdk-pixbuf; do
    for mo in "$prefix"/share/locale/*/LC_MESSAGES/$domain.mo; do
        [ -e "$mo" ] || continue
        install -Dm644 "$mo" "$root${mo}"
    done
done
strip --strip-unneeded "$root$prefix"/bin/* "$root$prefix"/lib/*.so.*

# GTK's own settings schemas live in the bundle, which GLib only searches when told.
cat > "$root/usr/bin/solstice" <<EOF
#!/bin/sh
export GSETTINGS_SCHEMA_DIR=$prefix/share/glib-2.0/schemas
# Debian only routes JACK clients to PipeWire when its library is on the path.
jack=/usr/lib/$triplet/pipewire-0.3/jack
[ -d "\$jack" ] && export LD_LIBRARY_PATH="\$jack\${LD_LIBRARY_PATH:+:\$LD_LIBRARY_PATH}"
exec $prefix/bin/solstice "\$@"
EOF
chmod 755 "$root/usr/bin/solstice"

install -Dm644 data/uk.co.solarflareit.Solstice.desktop "$root/usr/share/applications/uk.co.solarflareit.Solstice.desktop"
install -Dm644 data/uk.co.solarflareit.Solstice.metainfo.xml "$root/usr/share/metainfo/uk.co.solarflareit.Solstice.metainfo.xml"
install -Dm644 data/icons/uk.co.solarflareit.Solstice.svg "$root/usr/share/icons/hicolor/scalable/apps/uk.co.solarflareit.Solstice.svg"
install -Dm644 data/udev/70-solstice-cpu-latency.rules "$root/lib/udev/rules.d/70-solstice-cpu-latency.rules"
doc=$root/usr/share/doc/solstice
install -Dm644 README.md data/starter/CREDITS.md -t "$doc"
install -Dm644 LICENSE "$doc/copyright"
install -Dm644 vst3shim/vst3sdk/LICENSE.txt "$doc/LICENSE.vst3sdk"
install -Dm644 namshim/NeuralAmpModelerCore/LICENSE "$doc/LICENSE.NeuralAmpModelerCore"
for f in COPYING.README COPYING.MPL2 COPYING.APACHE COPYING.BSD; do
    install -Dm644 "namshim/NeuralAmpModelerCore/Dependencies/eigen/$f" "$doc/eigen/$f"
done
cat > "$doc/README.Debian" <<EOF
Solstice needs GTK 4.20 and libadwaita 1.8, newer than this Debian release ships, so this
package carries its own build of them, and of the GLib, Pango, Cairo, HarfBuzz, fontconfig,
graphene, gdk-pixbuf, Wayland, libxmlb and AppStream versions they need, in $prefix/lib.
Only Solstice uses them; the rest of the system keeps Debian's own.
EOF

# Debian libraries the binaries and the bundle link against, leaving out the bundle itself.
mkdir -p "$work/shlibs/debian"
printf 'Source: solstice\n\nPackage: solstice\nArchitecture: any\n' > "$work/shlibs/debian/control"
# dpkg-shlibdeps maps each bundled library to the Debian package of the same name, which
# Solstice never loads, so those packages are excluded.
exclude=$(for lib in "$root$prefix"/lib/*.so.*; do
    soname=$(objdump -p "$lib" | sed -n 's/^ *SONAME *//p')
    dpkg -S "/usr/lib/$triplet/$soname" "/lib/$triplet/$soname" 2>/dev/null | cut -d: -f1
done | sort -u | sed 's/^/-x/')
depends=$(cd "$work/shlibs" && dpkg-shlibdeps -O -l"$root$prefix/lib" --ignore-missing-info $exclude \
    "$root$prefix"/bin/* "$root$prefix"/lib/*.so.* 2>/dev/null | sed -n 's/^shlibs:Depends=//p')

mkdir -p "$root/DEBIAN"
cat > "$root/DEBIAN/control" <<EOF
Package: solstice
Version: $debversion
Architecture: amd64
Maintainer: SolarFlare Developments <$email>
Section: sound
Priority: optional
Homepage: https://solarflareit.co.uk
Depends: $depends, pipewire-jack, pipewire-bin, shared-mime-info, fontconfig-config, adwaita-icon-theme, hicolor-icon-theme
Installed-Size: $(du -sk "$root" | cut -f1)
Description: Digital audio workstation for GNOME
 Solstice is a digital audio workstation for GNOME with a professional studio layout,
 low-latency input monitoring through JACK or PipeWire, and VST3 and CLAP plug-in
 hosting.
EOF
cat > "$root/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
if [ "$1" = configure ]; then
    udevadm control --reload >/dev/null 2>&1 || true
    udevadm trigger --action=change --name-match=cpu_dma_latency >/dev/null 2>&1 || true
fi
EOF
chmod 755 "$root/DEBIAN/postinst"

mkdir -p "$out"
dpkg-deb --root-owner-group -Zxz --build "$root" "$out/solstice_${debversion}_amd64.deb"
rm -rf "$work"
