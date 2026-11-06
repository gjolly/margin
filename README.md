# Rune Diff

A Git diff viewer based on Rune Kit's diff example (MIT). Rune and Rune Kit
are pinned to the same Git revision; Cargo.lock and flake.lock pin dependencies.

## Build and run on NixOS

From this project:

    nix develop -c cargo build --locked
    nix develop -c cargo run --locked

The viewer runs Git in its current working directory. To inspect another
repository, enter the development shell and invoke the binary there:

    nix develop
    cargo build --locked
    export PATH="$PWD/target/debug:$PATH"
    cd /path/to/repository
    rune-diff
    rune-diff --cached
    rune-diff HEAD~1 HEAD
    rune-diff main...HEAD -- src/
    rune-diff --no-index old.txt new.txt

Arguments are passed directly to Git, preserving the -- path separator and
shell-quoted paths. With no arguments, this shows tracked working-tree changes
against the index, just like git diff. Untracked files are not included.
Split view is the default; the toolbar can switch to unified view.

The viewer controls patch formatting (no colors, external diff helpers or
textconv). Output-only modes such as --name-only, --raw, --check and --output
are rejected. Rune demo flags such as --scene and --snap are no longer accepted.
Git runs once after the first frame; this is a snapshot, with no automatic refresh.
Errors and empty results appear in the window. Text patches must be UTF-8;
binary changes show Git's binary-file summary.

## UI zoom and navigation

Ctrl++ (or Ctrl+=) enlarges the whole UI; Ctrl+- shrinks it.
Zoom changes in 10% steps from 50% to 200%; Ctrl+0 resets to 100%.
Zoom lasts for the current session. j/k navigate hunks; ]/[ navigate files.

## Development environment

The Nix shell supplies Rust, Git, the C linker, font libraries, Wayland/X11
libraries and the Vulkan loader. It configures library lookup, including the
NixOS host GPU drivers. Run from a desktop session with working Vulkan drivers.
The shell supports x86-64 and ARM64 Linux; only the setup host has been tested.

    nix develop -c cargo test --locked

## Distribution

The Nix development binary references the Nix store. For Ubuntu distribution,
build this Cargo project in Ubuntu CI or a container and package its runtime
dependencies. Ubuntu packaging is not configured yet.

## License

MIT; see LICENSE for the upstream attribution retained with the example.
