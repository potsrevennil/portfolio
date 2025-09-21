{
  description = "plonky-exp";
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-25.05";
    flake-parts = {
      url = "github:hercules-ci/flake-parts";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };
  };
  outputs = inputs@{ flake-parts, ... }: flake-parts.lib.mkFlake { inherit inputs; } {
    imports = [ ];
    systems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" "x86_64-darwin" ];
    perSystem = { pkgs, system, ... }:
      let
        wrapShell = mkShell: attrs:
          mkShell (attrs // {
            shellHook = ''
              export PATH=$PWD/scripts:$PATH
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
                ;
            };
        };

      };
  };
}
