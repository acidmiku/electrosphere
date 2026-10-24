{
  description = "Electrosphere: a native port of Ace Combat 3: Electrosphere (needs your own discs)";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forEachSystem =
        f:
        nixpkgs.lib.genAttrs systems (
          system:
          f (
            import nixpkgs {
              inherit system;
              # The package is marked unfree: the translated game code is not
              # ours to license (see README.md).
              config.allowUnfreePredicate = package: nixpkgs.lib.getName package == "electrosphere";
            }
          )
        );
    in
    {
      packages = forEachSystem (pkgs: rec {
        electrosphere = pkgs.callPackage ./nix/package.nix { };
        default = electrosphere;
      });

      apps = forEachSystem (pkgs: {
        default = {
          type = "app";
          program = "${self.packages.${pkgs.stdenv.hostPlatform.system}.default}/bin/ac3";
        };
      });

      devShells = forEachSystem (
        pkgs:
        let
          package = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
        in
        {
          default = pkgs.mkShell {
            inputsFrom = [ package ];
            packages = [
              pkgs.cargo
              pkgs.rustc
              pkgs.clippy
              pkgs.rustfmt
              pkgs.python3
            ];
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath package.passthru.runtimeLibraries;
          };
        }
      );

      formatter = forEachSystem (pkgs: pkgs.nixfmt);
    };
}
