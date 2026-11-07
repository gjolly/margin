{
  description = "Margin — native Git diff and review application";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forEachSystem = nixpkgs.lib.genAttrs systems;
      runtimeLibraries = pkgs: with pkgs; [
        fontconfig wayland libxkbcommon vulkan-loader
        libx11 libxcursor libxi libxrandr libxrender libxcb
      ];
    in {
      packages = forEachSystem (system:
        let
          pkgs = import nixpkgs { inherit system; };
          manifest = builtins.fromTOML (builtins.readFile ./Cargo.toml);
        in rec {
          default = margin;
          margin = pkgs.rustPlatform.buildRustPackage {
            pname = "margin";
            version = manifest.package.version;
            src = pkgs.lib.fileset.toSource {
              root = ./.;
              fileset = pkgs.lib.fileset.unions [ ./Cargo.toml ./Cargo.lock ./src ./LICENSE ];
            };
            cargoLock = {
              lockFile = ./Cargo.lock;
              # All Rune workspace crates come from this same Git tree.
              outputHashes."rune-0.0.1" = "sha256-Jq5kI6kEwQunO/kFVew47++8m21SCZzIvzXk5zZDsfc=";
            };
            nativeBuildInputs = [ pkgs.pkg-config pkgs.makeWrapper ];
            buildInputs = runtimeLibraries pkgs;
            nativeCheckInputs = [ pkgs.git ];
            postInstall = ''
              install -Dm644 LICENSE $out/share/licenses/margin/LICENSE
              wrapProgram $out/bin/margin \
                --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.git ]} \
                --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath (runtimeLibraries pkgs)}:/run/opengl-driver/lib
            '';
            meta = {
              description = "Native Git diff viewer with inline AI conversations";
              homepage = "https://github.com/gjolly/margin";
              license = pkgs.lib.licenses.mit;
              platforms = systems;
              mainProgram = "margin";
            };
          };
        });
      apps = forEachSystem (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/margin";
          meta.description = "Margin";
        };
      });
      devShells = forEachSystem (system:
        let
          pkgs = import nixpkgs { inherit system; };
        in {
          default = pkgs.mkShell {
            packages = with pkgs; [ cargo rustc pkg-config fontconfig git ];
            buildInputs = runtimeLibraries pkgs;
            shellHook = ''
              export LD_LIBRARY_PATH="${pkgs.lib.makeLibraryPath (runtimeLibraries pkgs)}:/run/opengl-driver/lib''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
            '';
          };
        });
    };
}
