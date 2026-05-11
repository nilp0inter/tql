{
  description = "tql — Tracker-Qualified Layout (Rust)";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      forAllSystems = f:
        nixpkgs.lib.genAttrs systems (system: f (import nixpkgs { inherit system; }));
    in
    {
      packages = forAllSystems (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "tql";
          version = "0.0.1";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = [ pkgs.pkg-config ];
          # No system libraries needed — reqwest uses rustls.
          # Tests touch the network/filesystem heavily; CI runs them via
          # `cargo test`. Skip them inside the Nix sandbox.
          doCheck = false;
          meta = with pkgs.lib; {
            description = "Tracker-Qualified Layout — organize qBittorrent downloads in a ghq-style tree.";
            homepage = "https://github.com/nilp0inter/tql";
            license = with licenses; [ mit asl20 ];
            mainProgram = "tql";
            platforms = systems;
          };
        };
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = [
            pkgs.cargo
            pkgs.rustc
            pkgs.rustfmt
            pkgs.clippy
            pkgs.gcc
            pkgs.pkg-config
          ];

          # rustls (via reqwest) needs no system openssl, but pkg-config is
          # cheap insurance for future C-dep additions.
          RUST_BACKTRACE = "1";
        };
      });

      formatter = forAllSystems (pkgs: pkgs.nixpkgs-fmt);
    };
}
