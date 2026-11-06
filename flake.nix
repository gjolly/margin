{
  description = "Rune diff viewer development environment";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forEachSystem = nixpkgs.lib.genAttrs systems;
    in {
      devShells = forEachSystem (system:
        let
          pkgs = import nixpkgs { inherit system; };
          runtimeLibraries = with pkgs; [
            wayland libxkbcommon vulkan-loader
            libx11 libxcursor libxi libxrandr libxrender libxcb
          ];
        in {
          default = pkgs.mkShell {
            packages = with pkgs; [ cargo rustc pkg-config fontconfig ];
            buildInputs = runtimeLibraries ++ [ pkgs.fontconfig ];
            shellHook = ''
              export LD_LIBRARY_PATH="${pkgs.lib.makeLibraryPath runtimeLibraries}:/run/opengl-driver/lib''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
            '';
          };
        });
    };
}
