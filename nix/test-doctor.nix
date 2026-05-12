{ pkgs, tqlModule, tqlPackage }:

# End-to-end check: `tql doctor --json` against a live qbittorrent-nox.
#
# Verifies that every static + runtime invariant exposed by `tql
# doctor` resolves green when the environment is healthy:
#   - config parses, paths exist + share a filesystem
#   - the example tracker bundle loads + its fixtures pass
#   - qbittorrent WebUI is reachable and credentials are accepted
#
# Then deliberately corrupts the runtime (wrong qbt password) and
# asserts that doctor flips to a non-zero exit + a fail entry on the
# auth check. This is the only DESIGN §7 subcommand still missing
# VM-level coverage.

let
  qbtPort = 8086;
  qbtPasswordHash =
    ''@ByteArray(ARQ77eY1NUZaQsuDHbIMCA==:0WMRkYTUWVT9wVvdDtHAjU9b3b7uB8NR1Gur2hmQCvCDpm39Q+PsJRJPaCU51dEiz+dTzh8qbPsL8WkFljQYFQ==)'';

  exampleTracker = pkgs.runCommand "tql-tracker-example" { } ''
    mkdir -p $out
    cp -r ${../trackers/example}/. $out/
  '';
in
pkgs.testers.runNixOSTest {
  name = "tql-doctor-e2e";

  nodes.machine = { config, lib, pkgs, ... }: {
    imports = [ tqlModule ];

    services.tql = {
      enable = true;
      package = tqlPackage;
      api.enable = true;
      readWritePaths = [ "/var/lib/tql" ];
      environmentFile = pkgs.writeText "tql-env" ''
        TQL_QBIT_PASSWORD=adminadmin
      '';
      settings = {
        paths = {
          seed_root = "/var/lib/tql/seed";
          library_root = "/var/lib/tql/library";
          trackers_root = "/var/lib/tql/trackers";
        };
        api.addr = "127.0.0.1:8080";
        qbittorrent = {
          url = "http://127.0.0.1:${toString qbtPort}";
          username = "admin";
          password_env = "TQL_QBIT_PASSWORD";
        };
      };
    };

    users.users.qbt = {
      isSystemUser = true;
      group = "qbt";
      home = "/var/lib/qbt";
      createHome = true;
    };
    users.groups.qbt = { };

    environment.etc."qbt/qBittorrent.conf".text = ''
      [LegalNotice]
      Accepted=true

      [Preferences]
      WebUI\Username=admin
      WebUI\Password_PBKDF2="${qbtPasswordHash}"
      WebUI\Port=${toString qbtPort}
      WebUI\Address=127.0.0.1
      WebUI\LocalHostAuth=true
      WebUI\CSRFProtection=false
      WebUI\HostHeaderValidation=false
    '';

    systemd.tmpfiles.rules = [
      "d /var/lib/tql/seed                       0755 tql tql -"
      "d /var/lib/tql/library                    0755 tql tql -"
      "d /var/lib/tql/trackers                   0755 tql tql -"
      "L+ /var/lib/tql/trackers/example          -    -   -   - ${exampleTracker}"
      "d /var/lib/qbt/qBittorrent                0755 qbt qbt -"
      "d /var/lib/qbt/qBittorrent/config         0755 qbt qbt -"
      "C /var/lib/qbt/qBittorrent/config/qBittorrent.conf 0644 qbt qbt - /etc/qbt/qBittorrent.conf"
    ];

    systemd.services.qbittorrent = {
      description = "qBittorrent (headless)";
      wantedBy = [ "multi-user.target" ];
      after = [ "network.target" ];
      serviceConfig = {
        User = "qbt";
        Group = "qbt";
        ExecStart = "${pkgs.qbittorrent-nox}/bin/qbittorrent-nox --profile=/var/lib/qbt --webui-port=${toString qbtPort}";
        Restart = "on-failure";
        RestartSec = 2;
      };
    };

    environment.systemPackages = [ pkgs.jq ];
  };

  testScript = ''
    import json

    start_all()
    machine.wait_for_unit("qbittorrent.service")
    machine.wait_for_open_port(${toString qbtPort})
    machine.wait_for_unit("tql-api.service")

    cfg = machine.succeed(
        "systemctl show -p Environment tql-api.service "
        "| sed -n 's/.*TQL_CONFIG=\\([^ ]*\\).*/\\1/p'"
    ).strip()
    print("config:", cfg)

    env_file = "${pkgs.writeText "tql-env-ok" ''
      TQL_QBIT_PASSWORD=adminadmin
    ''}"
    bad_env = "${pkgs.writeText "tql-env-bad" ''
      TQL_QBIT_PASSWORD=this-is-not-the-password
    ''}"

    # --- healthy environment -----------------------------------------
    out = machine.succeed(
        "systemd-run --pipe --wait --quiet --unit=tql-doctor-ok "
        "--uid=tql --gid=tql "
        f"--property=EnvironmentFile={env_file} "
        f"tql doctor --json --config {cfg}"
    )
    doc = json.loads(out)
    assert doc["exit_code"] == 0, doc
    assert doc["summary"]["fail"] == 0, doc
    by_name = {c["name"]: c for c in doc["checks"]}
    assert by_name["config"]["status"] == "ok", by_name["config"]
    assert by_name["paths.seed_root"]["status"] == "ok", by_name["paths.seed_root"]
    assert by_name["paths.library_root"]["status"] == "ok", by_name["paths.library_root"]
    assert by_name["paths.same_fs"]["status"] == "ok", by_name["paths.same_fs"]
    assert by_name["paths.metadata_dir"]["status"] == "ok", by_name["paths.metadata_dir"]
    assert by_name["trackers.root"]["status"] == "ok", by_name["trackers.root"]
    assert by_name["trackers.fixtures"]["status"] == "ok", by_name["trackers.fixtures"]
    assert by_name["qbittorrent.login"]["status"] == "ok", by_name["qbittorrent.login"]
    assert by_name["qbittorrent.version"]["status"] == "ok", by_name["qbittorrent.version"]
    # --probe was not passed; expect a warn placeholder, never a fail.
    assert by_name["probe"]["status"] == "warn", by_name["probe"]

    # Human-readable mode emits one line per check and a summary tail.
    human = machine.succeed(
        "systemd-run --pipe --wait --quiet --unit=tql-doctor-human "
        "--uid=tql --gid=tql "
        f"--property=EnvironmentFile={env_file} "
        f"tql doctor --config {cfg}"
    )
    assert "[OK  ]" in human, human
    assert "qbittorrent.login" in human, human
    assert "doctor:" in human, human

    # --- broken qbittorrent credentials ------------------------------
    rc, bad_out = machine.execute(
        "systemd-run --pipe --wait --quiet --unit=tql-doctor-bad "
        "--uid=tql --gid=tql "
        f"--property=EnvironmentFile={bad_env} "
        f"tql doctor --json --config {cfg}"
    )
    assert rc != 0, (rc, bad_out)
    bad_doc = json.loads(bad_out)
    assert bad_doc["exit_code"] == 1, bad_doc
    assert bad_doc["summary"]["fail"] >= 1, bad_doc
    bad_by_name = {c["name"]: c for c in bad_doc["checks"]}
    assert bad_by_name["qbittorrent.login"]["status"] == "fail", bad_by_name["qbittorrent.login"]
    # Static checks ahead of the qBittorrent probe should still pass.
    assert bad_by_name["trackers.fixtures"]["status"] == "ok", bad_by_name["trackers.fixtures"]
  '';
}
