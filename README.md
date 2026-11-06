# Rune Diff

A standalone starting point based on Rune Kit's diff example. Rune and Rune Kit
are pinned to the same Git revision; Cargo.lock and flake.lock pin dependencies.
The UI currently displays sample changes, with unified/split views and a review comment.

## Local development on NixOS

```sh
nix develop -c cargo build --locked
nix develop -c cargo run --locked -- --scene split --theme dark
```

Or enter `nix develop` once, then use `cargo build` and `cargo run` normally.
The shell supplies the compiler, C linker, font libraries, Wayland/X11 libraries
and Vulkan loader. It adds the NixOS host GPU driver directory to the library
search path. Run from a graphical desktop session with working Vulkan drivers.
The shell supports x86-64 and ARM64 Linux; validation is performed on the setup host.

To render a screenshot without opening a window (still requires a GPU backend):

```sh
nix develop -c cargo run --locked -- --scene split --theme dark --snap diff.png
```

The copied example includes placeholder Reply/Resolve buttons; it is a UI demo,
not yet a viewer for arbitrary Git repositories.

## Distribution

The Nix development binary references the Nix store. For Ubuntu distribution,
build the same Cargo project in Ubuntu CI or a container and package its runtime
dependencies. Ubuntu packaging is not configured yet.

## License

MIT; see LICENSE for the upstream attribution retained with the example.
