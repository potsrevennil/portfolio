{
  description = "plonky-exp";
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-26.05";
    flake-parts = {
      url = "github:hercules-ci/flake-parts";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };
  };
  outputs = inputs@{ flake-parts, ... }: flake-parts.lib.mkFlake { inherit inputs; } {
    imports = [ ];
    systems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" "x86_64-darwin" ];
    perSystem = { pkgs, system, self', ... }:
      let
        wrapShell = mkShell: attrs:
          mkShell (attrs // {
            shellHook = ''
              export PATH=$PWD/scripts:$PATH
              export RUSTC_WRAPPER=sccache
            '';
          });
      in
      {
        _module.args.pkgs = import inputs.nixpkgs {
          inherit system;
        };

        devShells.default = wrapShell pkgs.mkShellNoCC {
          packages =
            builtins.attrValues {
              inherit (pkgs)
                direnv
                nix-direnv

                nixpkgs-fmt
                deadnix
                shfmt
                shellcheck
                rustup
                clang
                taplo
                codespell
                gitleaks
                protobuf
                sqlx-cli
                sqlite
                sccache
                # LINE Bank statements are PDFs; the importer reads them through
                # `pdftotext -raw`.
                poppler-utils

                # Web UI build (crates/web): cargo-leptos drives the server and wasm
                # builds. It runs the wasm-bindgen-cli on PATH, which must match
                # the wasm-bindgen crates/web/Cargo.toml pins; without one it
                # downloads its own. binaryen's wasm-opt shrinks the release wasm.
                cargo-leptos
                wasm-bindgen-cli_0_2_100
                binaryen

                beancount
                hledger
                fava
                ;
            };
        };

        # The browser journeys (crates/web/tests/browser.rs): a headless
        # Chromium and the chromedriver built for the same version. Linux only,
        # and a shell of its own so everyday work does not download a browser.
        devShells.browser = wrapShell pkgs.mkShellNoCC {
          inputsFrom = [ self'.devShells.default ];
          packages = pkgs.lib.optionals pkgs.stdenv.isLinux [
            pkgs.chromium
            pkgs.chromedriver
          ];
        };
      };
  };
}
