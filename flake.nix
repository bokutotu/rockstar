{
  description = "A minimal GPUI chat client for pi with Markdown rendering";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      configFor =
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          libraries = with pkgs; [
            fontconfig
            freetype
            libGL
            libx11
            libxcb
            libxkbcommon
            vulkan-loader
            wayland
          ];
          nativeBuildInputs = [ pkgs.pkg-config ];
          fontconfigFile = pkgs.makeFontsConf {
            fontDirectories = with pkgs; [
              dejavu_fonts
              (noto-fonts-cjk-sans.override { static = true; })
            ];
          };
          testSetup = ''
            export FONTCONFIG_FILE=${fontconfigFile}
            export LD_LIBRARY_PATH=${pkgs.lib.makeLibraryPath libraries}
            export VK_DRIVER_FILES=${pkgs.mesa}/share/vulkan/icd.d/lvp_icd.${pkgs.stdenv.hostPlatform.parsed.cpu.name}.json
            export __EGL_VENDOR_LIBRARY_FILENAMES=${pkgs.mesa}/share/glvnd/egl_vendor.d/50_mesa.json
            export LIBGL_ALWAYS_SOFTWARE=1
            unset DISPLAY WAYLAND_DISPLAY
            export XDG_RUNTIME_DIR
            XDG_RUNTIME_DIR="$(mktemp -d)"
          '';
          testRunner = pkgs.writeShellApplication {
            name = "test-visual";
            runtimeInputs = [
              pkgs.cargo
              pkgs.coreutils
            ];
            text = testSetup + ''
              trap 'rm -rf "$XDG_RUNTIME_DIR"' EXIT
              cargo test --locked "$@"
            '';
          };
        in
        {
          inherit
            pkgs
            libraries
            nativeBuildInputs
            fontconfigFile
            testSetup
            testRunner
            ;
        };
    in
    {
      packages = forAllSystems (
        system:
        let
          inherit (configFor system)
            pkgs
            libraries
            nativeBuildInputs
            fontconfigFile
            testSetup
            ;
        in
        {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "rockstar";
            version = "0.1.0";
            src = pkgs.lib.cleanSource ./.;
            cargoLock.lockFile = ./Cargo.lock;

            nativeBuildInputs = nativeBuildInputs ++ [ pkgs.makeWrapper ];
            buildInputs = libraries;
            preCheck = testSetup;

            postFixup = ''
              wrapProgram "$out/bin/rockstar" \
                --prefix LD_LIBRARY_PATH : "${pkgs.lib.makeLibraryPath libraries}" \
                --prefix PATH : "${pkgs.lib.makeBinPath [ pkgs.xdg-utils ]}" \
                --set FONTCONFIG_FILE "${fontconfigFile}"
            '';

            meta = {
              description = "A minimal GPUI chat client for pi";
              mainProgram = "rockstar";
              platforms = systems;
            };
          };
        }
      );

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/rockstar";
          meta.description = "Chat with pi in the current directory";
        };
      });

      devShells = forAllSystems (
        system:
        let
          inherit (configFor system)
            pkgs
            libraries
            nativeBuildInputs
            fontconfigFile
            testRunner
            ;
        in
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              clippy
              rust-analyzer
              rustc
              rustfmt
              nixfmt
              xdg-utils
              testRunner
            ];
            inherit nativeBuildInputs;
            buildInputs = libraries;
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath libraries;
            FONTCONFIG_FILE = fontconfigFile;
          };
        }
      );

      checks = forAllSystems (system: {
        default = self.packages.${system}.default;
      });

      formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.nixfmt);
    };
}
