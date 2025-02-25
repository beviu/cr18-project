{
  description = "Development tools for the CR18 project";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    treefmt-nix = {
      url = "github:numtide/treefmt-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { nixpkgs, treefmt-nix, ... }:
    let
      forAllSystems = nixpkgs.lib.genAttrs nixpkgs.lib.systems.flakeExposed;
    in
    {
      devShells = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
        in
        {
          default = pkgs.mkShell {
            packages = [
              pkgs.bpftools
              pkgs.cargo
              pkgs.clang-tools
              pkgs.clang
              pkgs.clippy
              pkgs.elfutils
              pkgs.glibc_multi
              pkgs.libcap
              pkgs.libmnl
              pkgs.libpcap
              pkgs.llvm
              pkgs.m4
              pkgs.pciutils
              pkgs.pkg-config
              pkgs.pktgen
              pkgs.rustfmt
            ];

            # Disable zerocallusedregs to fix an error when bulding BPF programs.
            NIX_HARDENING_ENABLE = "";
          };
        }
      );

      formatter = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          treefmtEval = treefmt-nix.lib.evalModule pkgs ./treefmt.nix;
        in
        treefmtEval.config.build.wrapper
      );
    };
}
