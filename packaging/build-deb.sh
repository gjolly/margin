#!/usr/bin/env bash
# Run on the target Ubuntu release after cargo build --release --locked.
set -euo pipefail

project_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
binary=$(realpath "${1:-$project_dir/target/release/margin}")
output_dir=${2:-$project_dir/target/debian}
mkdir -p "$output_dir"
output_dir=$(realpath "$output_dir")
version=${DEB_VERSION:-$(python3 -c 'import sys, tomllib; print(tomllib.load(open(sys.argv[1], "rb"))["package"]["version"] + "-1")' "$project_dir/Cargo.toml")}
dpkg --validate-version "$version"
architecture=$(dpkg --print-architecture)

# Nix-built executables cannot be distributed as native Ubuntu binaries.
elf_metadata=$(readelf -l -d "$binary")
if [[ "$elf_metadata" == */nix/store* ]]; then
  echo "Build the binary natively on Ubuntu, outside Nix." >&2
  exit 1
fi

work_dir=$(mktemp -d)
trap 'rm -rf -- "$work_dir"' EXIT
package_dir=$work_dir/package
install -Dm755 "$binary" "$package_dir/usr/bin/margin"
strip --strip-unneeded "$package_dir/usr/bin/margin"
install -Dm644 "$project_dir/LICENSE" "$package_dir/usr/share/doc/margin/copyright"
install -Dm644 "$project_dir/README.md" "$package_dir/usr/share/doc/margin/README.md"

# dpkg-shlibdeps requires package metadata even for a binary-only build.
mkdir -p "$work_dir/debian" "$package_dir/DEBIAN"
cat > "$work_dir/debian/control" <<'EOF'
Source: margin
Section: devel
Priority: optional
Maintainer: Margin contributors <gjolly@users.noreply.github.com>

Package: margin
Architecture: any
Description: Native Git diff and review application
EOF
linked_dependencies=$(cd "$work_dir" && dpkg-shlibdeps -O -e"$package_dir/usr/bin/margin")
linked_dependencies=${linked_dependencies#shlibs:Depends=}
if [[ -z "$linked_dependencies" || "$linked_dependencies" == *$'\n'* ]]; then
  echo "Could not determine linked runtime dependencies." >&2
  exit 1
fi

# Display libraries are loaded with dlopen. libwayland-cursor0 pulls in the
# Wayland client; the four X11 alternatives together provide its complete stack.
# Repeating the Wayland alternative expresses Wayland OR (all four X11 libs),
# so an installed X11 stack satisfies every clause without pulling in Wayland.
# XRandR uses the XCB protocol directly and does not need libXrandr.
display_dependencies="libwayland-cursor0 | libx11-xcb1, libwayland-cursor0 | libxcursor1, libwayland-cursor0 | libxi6, libwayland-cursor0 | libxkbcommon-x11-0"
cat > "$package_dir/DEBIAN/control" <<EOF
Package: margin
Version: $version
Architecture: $architecture
Section: devel
Priority: optional
Maintainer: Margin contributors <gjolly@users.noreply.github.com>
Homepage: https://github.com/gjolly/margin
Installed-Size: $(du -sk "$package_dir/usr" | cut -f1)
Depends: $linked_dependencies, git, libfontconfig1, libxkbcommon0, libvulkan1, $display_dependencies
Recommends: fonts-dejavu-core
Description: Native Git diff and review application
 Read-only Git diff viewer with split and unified views and optional inline
 AI conversations using an OpenAI-compatible endpoint.
EOF

dpkg-deb --root-owner-group --build "$package_dir" "$output_dir/margin_${version}_${architecture}.deb"
