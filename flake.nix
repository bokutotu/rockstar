{
  description = "ROCKSTAR BY DaBaby";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { nixpkgs, ... }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      # Match aoewif's GHC/HLS toolchain, including its GHC 9.14 compatibility fixes.
      mkHaskellPackages =
        pkgs:
        let
          hlib = pkgs.haskell.lib;
          disableFlags =
            flags: package: builtins.foldl' (result: flag: hlib.disableCabalFlag result flag) package flags;
        in
        pkgs.haskell.packages.ghc9141.override {
          overrides = self: super: {
            algebraic-graphs = super.algebraic-graphs_0_8;
            constraints-extras = hlib.doJailbreak super.constraints-extras;
            dependent-map = hlib.doJailbreak super.dependent-map;
            enummapset = hlib.dontCheck super.enummapset;
            generic-lens = hlib.dontCheck super.generic-lens;
            ghcide = hlib.doJailbreak super.ghcide;
            ghc-trace-events = hlib.doJailbreak super.ghc-trace-events;
            hiedb = hlib.dontCheck (hlib.doJailbreak super.hiedb);
            hie-compat = hlib.doJailbreak super.hie-compat;
            lucid = hlib.doJailbreak super.lucid;
            lsp = hlib.doJailbreak super.lsp;
            lsp-test = hlib.doJailbreak super.lsp-test;
            lsp-types = hlib.doJailbreak super.lsp-types;
            ordered-containers = hlib.doJailbreak super.ordered-containers;
            rebase = hlib.doJailbreak super.rebase;
            regex-tdfa = hlib.dontCheck super.regex-tdfa;
            string-interpolate = hlib.doJailbreak super.string-interpolate;
            tasty-hspec = hlib.doJailbreak super.tasty-hspec;
            toml-reader = hlib.dontCheck super.toml-reader;
            weeder = hlib.justStaticExecutables (hlib.dontCheck (hlib.doJailbreak super.weeder));
            haskell-language-server =
              hlib.overrideCabal
                (disableFlags
                  [
                    "cabal"
                    "cabalfmt"
                    "cabalgild"
                    "floskell"
                    "fourmolu"
                    "ghc-lib"
                    "hlint"
                    "ormolu"
                    "retrie"
                    "splice"
                    "stan"
                    "stylishHaskell"
                  ]
                  (
                    super.haskell-language-server.overrideScope (
                      _: _: {
                        Cabal = null;
                        Cabal-syntax = null;
                        apply-refact = null;
                        cabal-add = null;
                        eventlog2html = null;
                        fourmolu = null;
                        hlint = null;
                        ormolu = null;
                        refact = null;
                        shake-bench = null;
                        stan = null;
                        stylish-haskell = null;
                      }
                    )
                  )
                )
                (_: {
                  buildDepends = [ ];
                });
          };
        };
    in
    {
      devShells = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          haskellPackages = mkHaskellPackages pkgs;
          ghc = haskellPackages.ghcWithPackages (h: [
            h.aeson
            h.async
            h.base64-bytestring
            h.crypton
            h.entropy
            h.hspec
            h.http-client
            h.http-client-tls
            h.http-types
            h.memory
            h.network
            h.optparse-applicative
            h.temporary
            h.wai
            h.warp
          ]);
          hls = pkgs.writeShellScriptBin "hls" ''
            exec ${haskellPackages.haskell-language-server}/bin/haskell-language-server-wrapper "$@"
          '';
          haskellFormat = pkgs.writeShellApplication {
            name = "format";
            runtimeInputs = [
              pkgs.fourmolu
              pkgs.stylish-haskell
            ];
            text = ''
              if (( $# == 0 )); then
                shopt -s globstar nullglob
                set -- {app,src,test}/**/*.{hs,lhs,hs-boot,lhs-boot}
              fi

              if (( $# > 0 )); then
                fourmolu --mode inplace "$@"
                stylish-haskell --inplace "$@"
              fi
            '';
          };
        in
        {
          # Keep the C toolchain for the POSIX CApiFFI imports in credential storage.
          default = pkgs.mkShell {
            packages = [
              ghc
              pkgs.cabal-install
              pkgs.haskellPackages.fast-tags
              pkgs.fourmolu
              pkgs.haskellPackages.hpack
              haskellPackages.haskell-language-server
              haskellPackages.weeder
              pkgs.stylish-haskell
              pkgs.hlint
              hls
              haskellFormat
            ];
          };
        }
      );

      formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.nixfmt);
    };
}
