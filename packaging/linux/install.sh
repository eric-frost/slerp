#!/bin/sh
# Installs Slerp from this folder, on Linux distributions without a Slerp
# package.  Run it as root:
#
#   ./install.sh                  into /usr/local
#   ./install.sh --prefix /usr    into /usr, where GNOME's file managers need
#                                 it: their thumbnail sandbox sees only /usr
#   ./install.sh --uninstall      removes it again (with the same --prefix)
#
# It copies bin/ and share/ under the prefix, updates the MIME and menu
# caches, and puts the Qt image plugin (qt-plugin/) into Qt 6's own plugin
# folder when Qt 6 is installed, since Qt looks nowhere else.  The program
# needs the Vulkan loader (libvulkan1 or vulkan-loader), and Mesa's Vulkan
# drivers draw 3D thumbnails where there is no GPU driver.
set -eu
cd "$(dirname "$0")"
prefix=/usr/local
mode=install
while [ $# -gt 0 ]; do
	case $1 in
	--prefix) prefix=$2; shift 2 ;;
	--prefix=*) prefix=${1#*=}; shift ;;
	--uninstall) mode=uninstall; shift ;;
	*) echo "usage: ./install.sh [--prefix DIR] [--uninstall]" >&2; exit 2 ;;
	esac
done

# Qt's plugin folder, from Qt's tools when they are installed, else the
# usual places that hold Qt's own image plugins.
plugins=$(qtpaths6 --query QT_INSTALL_PLUGINS 2>/dev/null || qmake6 -query QT_INSTALL_PLUGINS 2>/dev/null || true)
if [ -z "$plugins" ]; then
	for dir in /usr/lib/*/qt6/plugins /usr/lib64/qt6/plugins /usr/lib/qt6/plugins; do
		if [ -d "$dir/imageformats" ]; then plugins=$dir; break; fi
	done
fi
qt=""

for file in $(find bin share -type f | sort); do
	if [ "$mode" = install ]; then
		case $file in bin/*) perm=755 ;; *) perm=644 ;; esac
		install -D -m "$perm" "$file" "$prefix/$file"
	else
		rm -f "$prefix/$file"
	fi
done
if [ -n "$plugins" ] && [ -f qt-plugin/slerp_qimage.so ]; then
	qt=$plugins/imageformats/slerp_qimage.so
	if [ "$mode" = install ]; then
		install -D -m 644 qt-plugin/slerp_qimage.so "$qt"
	else
		rm -f "$qt"
	fi
fi
[ "$mode" = install ] || rmdir "$prefix/share/doc/slerp" "$prefix/share/thumbnailers" 2>/dev/null || true

# The caches the MIME types, the menu entry and the icon go into.
if command -v update-mime-database >/dev/null; then update-mime-database "$prefix/share/mime"; fi
if command -v update-desktop-database >/dev/null; then update-desktop-database -q "$prefix/share/applications"; fi
if command -v gtk-update-icon-cache >/dev/null && [ -f "$prefix/share/icons/hicolor/index.theme" ]; then
	gtk-update-icon-cache -q "$prefix/share/icons/hicolor" || true
fi

if [ "$mode" = install ]; then
	echo "Installed Slerp in $prefix${qt:+, and its Qt plugin as $qt}."
	echo "For thumbnails in Dolphin, each user runs once: slerp associate --apply"
else
	echo "Removed Slerp from $prefix."
fi
