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
          cargoHash = "sha256-7lL/OLlq8znSXm95nEnjs93jM0QEHsH7WnIHH+pdkKk=";
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

      checks = forAllSystems (pkgs:
        nixpkgs.lib.optionalAttrs pkgs.stdenv.isLinux {
          # Boot a NixOS VM with `services.tql.enable = true` and verify the
          # tql-api unit comes up and answers /health. End-to-end coverage for
          # nix/module.nix (Leg 19) — flake.nix only exposes it on Linux
          # because nixosTest requires KVM/QEMU.
          nixos-module = import ./nix/test-module.nix {
            inherit pkgs;
            tqlModule = self.nixosModules.default;
            tqlPackage = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
          };
          # End-to-end: tql in a NixOS VM talking to a real qbittorrent-nox.
          nixos-qbittorrent = import ./nix/test-qbittorrent.nix {
            inherit pkgs;
            tqlModule = self.nixosModules.default;
            tqlPackage = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
          };
          # End-to-end: `tql cli example ...` submits a real .torrent to
          # the live qbittorrent and the resulting state matches the
          # classifier's link_tags.
          nixos-cli = import ./nix/test-cli.nix {
            inherit pkgs;
            tqlModule = self.nixosModules.default;
            tqlPackage = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
          };
          # End-to-end: cli submission → qbittorrent → synthetic
          # download completion → `tql post-process` → sidecar +
          # hardlinks under library_root match the classifier output.
          nixos-post-process = import ./nix/test-post-process.nix {
            inherit pkgs;
            tqlModule = self.nixosModules.default;
            tqlPackage = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
          };
          # End-to-end: notify-flush dispatches a rendered Telegram payload
          # to a local HTTP sink, exercising the per-tracker template
          # override from `trackers/example/notify.hbs`.
          nixos-notify = import ./nix/test-notify.nix {
            inherit pkgs;
            tqlModule = self.nixosModules.default;
            tqlPackage = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
          };
          # End-to-end: `tql reconcile` against live qbittorrent rebuilds
          # the sidecar + hardlinks from torrent state alone (no prior
          # post-process invocation).
          nixos-reconcile = import ./nix/test-reconcile.nix {
            inherit pkgs;
            tqlModule = self.nixosModules.default;
            tqlPackage = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
          };
          # End-to-end: `tql api` REST surface accepts a JSON submit
          # against POST /trackers/example/add and lands the torrent in
          # the live qbittorrent with the classifier-derived tags.
          nixos-api = import ./nix/test-api.nix {
            inherit pkgs;
            tqlModule = self.nixosModules.default;
            tqlPackage = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
          };
          # End-to-end: `tql api` with `[api].api_key_env` set, verifying
          # the bearer / X-Api-Key auth gate via the wire under a
          # hardened systemd unit. No qbittorrent required.
          nixos-api-auth = import ./nix/test-api-auth.nix {
            inherit pkgs;
            tqlModule = self.nixosModules.default;
            tqlPackage = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
          };
          # End-to-end: `tql link add` / `tql link remove` mutate
          # qBittorrent tags and reapply the post-process pipeline so
          # the on-disk hardlink sites + sidecar reflect the new tag
          # set.
          nixos-link = import ./nix/test-link.nix {
            inherit pkgs;
            tqlModule = self.nixosModules.default;
            tqlPackage = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
          };
          # End-to-end: `tql doctor --json` against a live qbittorrent
          # — every static + runtime invariant resolves green, and a
          # broken qbt credential flips the auth check to fail with a
          # non-zero exit.
          nixos-doctor = import ./nix/test-doctor.nix {
            inherit pkgs;
            tqlModule = self.nixosModules.default;
            tqlPackage = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
          };
          # End-to-end: read-only `tql sidecar list/show/verify`
          # against a sidecar built by reconcile on top of a live
          # qbittorrent — both clean state and a broken hardlink.
          nixos-sidecar = import ./nix/test-sidecar.nix {
            inherit pkgs;
            tqlModule = self.nixosModules.default;
            tqlPackage = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
          };
          # End-to-end: mutating `tql sidecar repair` (rebuild a
          # broken hardlink) and `tql sidecar gc` (drop an orphan
          # sidecar + its hardlinks after the qBittorrent torrent
          # is removed). Both exercised in --dry-run then for real.
          nixos-sidecar-mutate = import ./nix/test-sidecar-mutate.nix {
            inherit pkgs;
            tqlModule = self.nixosModules.default;
            tqlPackage = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
          };
          # End-to-end: `tql reload` against a live `tql api` —
          # validates trackers_root, discovers /run/tql/api.pid,
          # delivers SIGHUP, and the api logs the registry swap.
          # Also covers the no-server branch (api stopped → exit 0 +
          # `no_server`).
          nixos-reload = import ./nix/test-reload.nix {
            inherit pkgs;
            tqlModule = self.nixosModules.default;
            tqlPackage = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
          };
          # End-to-end: `tql mcp --http` against a live qbittorrent —
          # JSON-RPC `initialize` / `tools/list` / `tools/call` for
          # `tracker.example.add` lands a .torrent with the
          # classifier-derived category + tags. Covers DESIGN §7's MCP
          # transport through `services.tql.mcp`.
          nixos-mcp = import ./nix/test-mcp.nix {
            inherit pkgs;
            tqlModule = self.nixosModules.default;
            tqlPackage = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
          };
          # Pure-eval check: Home Manager module renders the expected
          # user-scoped systemd units without pulling in home-manager.
          home-module = import ./nix/test-home-module.nix {
            inherit pkgs;
            tqlHomeModule = self.homeManagerModules.default;
            tqlPackage = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
          };
        });

      nixosModules.default = { pkgs, ... }: {
        imports = [ ./nix/module.nix ];
        # Default the package to this flake's build for the host system.
        services.tql.package = nixpkgs.lib.mkDefault
          self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      };
      nixosModules.tql = self.nixosModules.default;

      homeManagerModules.default = { pkgs, ... }: {
        imports = [ ./nix/home-module.nix ];
        services.tql.package = nixpkgs.lib.mkDefault
          self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      };
      homeManagerModules.tql = self.homeManagerModules.default;
    };
}
