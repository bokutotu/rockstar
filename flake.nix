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
          renderEnvironment = {
            FONTCONFIG_FILE = fontconfigFile;
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath libraries;
            VK_DRIVER_FILES = "${pkgs.mesa}/share/vulkan/icd.d/lvp_icd.${pkgs.stdenv.hostPlatform.parsed.cpu.name}.json";
            __EGL_VENDOR_LIBRARY_FILENAMES = "${pkgs.mesa}/share/glvnd/egl_vendor.d/50_mesa.json";
            LIBGL_ALWAYS_SOFTWARE = "1";
          };
        in
        {
          inherit
            pkgs
            libraries
            nativeBuildInputs
            fontconfigFile
            renderEnvironment
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
            renderEnvironment
            ;
        in
        {
          default = pkgs.rustPlatform.buildRustPackage (
            renderEnvironment
            // {
              pname = "rockstar";
              version = "0.1.0";
              src = pkgs.lib.cleanSource ./.;
              cargoLock.lockFile = ./Cargo.lock;

              nativeBuildInputs = nativeBuildInputs ++ [ pkgs.makeWrapper ];
              buildInputs = libraries;
              preCheck = ''
                unset DISPLAY WAYLAND_DISPLAY
                export XDG_RUNTIME_DIR
                XDG_RUNTIME_DIR="$(mktemp -d)"
              '';

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
            }
          );
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
            renderEnvironment
            ;
        in
        {
          default = pkgs.mkShell (
            renderEnvironment
            // {
              packages = with pkgs; [
                cargo
                clippy
                rust-analyzer
                rustc
                rustfmt
                nixfmt
                xdg-utils
              ];
              inherit nativeBuildInputs;
              buildInputs = libraries;
            }
          );
        }
      );

      checks = forAllSystems (system: {
        default = self.packages.${system}.default;
      });

      formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.nixfmt);
    };
}
