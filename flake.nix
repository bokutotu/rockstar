{
  description = "A tiled GPUI workspace for pi chat and source editing";

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
              nerd-fonts.jetbrains-mono
              (noto-fonts-cjk-sans.override { static = true; })
              noto-fonts-color-emoji
            ];
          };
          renderEnvironment = {
            FONTCONFIG_FILE = fontconfigFile;
            # Use the host GPU driver for both the application and cargo test.
            LD_LIBRARY_PATH = "${pkgs.lib.makeLibraryPath libraries}:/run/opengl-driver/lib";
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
              # Tests are run directly with cargo test, not inside the build sandbox.
              doCheck = false;

              postFixup = ''
                wrapProgram "$out/bin/rockstar" \
                  --prefix LD_LIBRARY_PATH : "${renderEnvironment.LD_LIBRARY_PATH}" \
                  --prefix PATH : "${
                    pkgs.lib.makeBinPath [
                      pkgs.xdg-utils
                      pkgs.gitMinimal
                    ]
                  }" \
                  --set FONTCONFIG_FILE "${fontconfigFile}"
              '';

              meta = {
                description = "A tiled workspace for pi chat and source editing";
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
          meta.description = "Open a chat and source workspace in the current directory";
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
                nixd
                nixfmt
                gitMinimal
                xdg-utils
                xdotool
                python3
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
