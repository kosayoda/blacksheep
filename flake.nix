{
  description = "Wayland shell";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (pkgs: rec {
        blacksheep = pkgs.callPackage ./nix/package.nix { };
        default = blacksheep;
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          inputsFrom = [ self.packages.${pkgs.stdenv.hostPlatform.system}.blacksheep ];
          packages = with pkgs; [ cargo rustc rust-analyzer clippy rustfmt just ];
          # After the Qt setup hooks, which export their own QMAKE.
          shellHook = ''
            export QMAKE=${self.packages.${pkgs.stdenv.hostPlatform.system}.blacksheep.qmake}
          '';
        };
      });
    };
}
