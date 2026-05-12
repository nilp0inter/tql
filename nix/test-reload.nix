{ pkgs, tqlModule, tqlPackage }:

# End-to-end check: `tql reload` against a live `tql api` service.
#
# Exercises DESIGN §7's reload contract through the systemd unit
# the NixOS module materializes:
#   - `tql api` writes /run/tql/api.pid on startup.
#   - `tql reload --json` validates trackers_root, discovers the
#     PID file, sends SIGHUP, and the running api process logs
#     "SIGHUP — reloading registry" + "registry reloaded".
#   - With no running server, reload exits 0 + emits `no_server`.
#
# Complements the unit tests in `src/cmd/reload.rs`, which cover the
# JSON shape and the stale-PID branch but never crossed the wire to
# a real systemd unit. qbittorrent is intentionally absent — reload
# doesn't talk to qBit, and skipping it keeps this VM lean (~25 s).

let
  apiPort = 8090;

  exampleTracker = pkgs.runCommand "tql-tracker-example" { } ''
    mkdir -p $out
    cp -r ${../trackers/example}/. $out/
  '';
in
pkgs.testers.runNixOSTest {
  name = "tql-reload-e2e";

  nodes.machine = { config, lib, pkgs, ... }: {
    imports = [ tqlModule ];

    services.tql = {
      enable = true;
      package = tqlPackage;
      api.enable = true;
      readWritePaths = [ "/var/lib/tql" ];
      settings = {
        paths = {
          seed_root = "/var/lib/tql/seed";
          library_root = "/var/lib/tql/library";
          trackers_root = "/var/lib/tql/trackers";
        };
        api.addr = "127.0.0.1:${toString apiPort}";
      };
    };

    systemd.tmpfiles.rules = [
      "d /var/lib/tql/seed                       0755 tql tql -"
      "d /var/lib/tql/library                    0755 tql tql -"
      "d /var/lib/tql/trackers                   0755 tql tql -"
      "L+ /var/lib/tql/trackers/example          -    -   -   - ${exampleTracker}"
    ];

    environment.systemPackages = [ pkgs.jq ];
  };

  testScript = ''
    import json

    start_all()
    machine.wait_for_unit("tql-api.service")
    machine.wait_for_open_port(${toString apiPort})

    cfg = machine.succeed(
        "systemctl show -p Environment tql-api.service "
        "| sed -n 's/.*TQL_CONFIG=\\([^ ]*\\).*/\\1/p'"
    ).strip()
    print("config:", cfg)

    # /run/tql is the RuntimeDirectory; api.pid must land there.
    machine.succeed("test -e /run/tql/api.pid")
    owner = machine.succeed("stat -c '%U:%G' /run/tql/api.pid").strip()
    assert owner == "tql:tql", owner

    api_pid_file = int(machine.succeed("cat /run/tql/api.pid").strip())
    main_pid = int(machine.succeed(
        "systemctl show -p MainPID --value tql-api.service"
    ).strip())
    assert api_pid_file == main_pid, (api_pid_file, main_pid)

    # --- live reload: SIGHUP delivered, registry reloaded ------------
    # systemd-run inherits neither RuntimeDirectory nor the unit's
    # environment, so propagate TQL_RUN_DIR explicitly. Same
    # --uid/--gid pattern siblings use to pin the operator path.
    out = machine.succeed(
        "systemd-run --pipe --wait --quiet --unit=tql-reload-live "
        "--uid=tql --gid=tql "
        "--setenv=TQL_RUN_DIR=/run/tql "
        f"tql reload --json --config {cfg}"
    )
    print("reload ok:", out)
    r = json.loads(out)
    assert r["outcome"] == "ok", r
    assert r["no_server"] is False, r
    assert r["validated"] >= 1, r
    assert r["load_failures"] == [], r
    assert r["errors"] == [], r
    sigs = r["signaled"]
    assert len(sigs) == 1, r
    assert sigs[0]["role"] == "api", sigs[0]
    assert sigs[0]["pid"] == main_pid, (sigs[0], main_pid)

    # The running api should have logged the reload. Give the signal
    # handler a beat — SIGHUP delivery is async w.r.t. the kill(2).
    machine.wait_until_succeeds(
        "journalctl -u tql-api.service --no-pager "
        "| grep -q 'SIGHUP — reloading registry'",
        timeout=10,
    )
    machine.succeed(
        "journalctl -u tql-api.service --no-pager "
        "| grep -q 'registry reloaded'"
    )

    # PID file survived RuntimeDirectoryPreserve across the SIGHUP, and
    # /trackers still serves post-reload.
    assert int(machine.succeed("cat /run/tql/api.pid").strip()) == main_pid
    trackers = json.loads(machine.succeed(
        "curl -fsS http://127.0.0.1:${toString apiPort}/trackers"
    ))
    names = {t["name"] for t in trackers["trackers"]}
    assert "example" in names, trackers

    # --- human-mode reload prints the signal line --------------------
    human = machine.succeed(
        "systemd-run --pipe --wait --quiet --unit=tql-reload-human "
        "--uid=tql --gid=tql "
        "--setenv=TQL_RUN_DIR=/run/tql "
        f"tql reload --config {cfg}"
    )
    assert "sent SIGHUP to api" in human, human
    assert f"pid={main_pid}" in human, human

    # --- no-server branch: stop the api, reload exits 0 + no_server --
    machine.succeed("systemctl stop tql-api.service")
    machine.succeed("test ! -e /run/tql/api.pid || rm -f /run/tql/api.pid")

    out = machine.succeed(
        "systemd-run --pipe --wait --quiet --unit=tql-reload-empty "
        "--uid=tql --gid=tql "
        "--setenv=TQL_RUN_DIR=/run/tql "
        f"tql reload --json --config {cfg}"
    )
    r = json.loads(out)
    assert r["outcome"] == "ok", r
    assert r["no_server"] is True, r
    assert r["signaled"] == [], r
    assert r["errors"] == [], r
  '';
}
