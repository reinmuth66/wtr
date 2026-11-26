{
  description = "wtr: a TUI file viewer";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "aarch64-darwin"
        "x86_64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "wtr";
          version = "0.1.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          # nixpkgs builds onig_sys (via syntect) against the system oniguruma.
          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = [ pkgs.oniguruma ];
          nativeCheckInputs = [ pkgs.delta ];
          # The watch tests rely on FSEvents, which is unreliable inside the nix build.
          checkFlags = [
            "--skip"
            "watch::"
          ];
        };
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            rustc
            clippy
            rustfmt
            rust-analyzer
            delta
          ];
        };
      });
    };
}
