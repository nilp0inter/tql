{ pkgs, tqlModule, tqlPackage }:

# End-to-end check: boot a NixOS VM with a real qbittorrent-nox daemon,
# configure tql against it, and prove `tql doctor --json` reports the
# qbittorrent login probe as "ok". Complements the smoke check in
# `test-module.nix` (which only exercises the bundled HTTP server).
#
# qBittorrent's WebUI is pre-seeded with the well-known
# `admin` / `adminadmin` credential pair via the canonical PBKDF2 hash
# from the upstream wiki, so no first-run wizard is needed.

let
  qbtPort = 8082;
  # PBKDF2-SHA512 of "adminadmin" using the salt baked into qBittorrent's
  # own example configs. Documented at
  # https://github.com/qbittorrent/qBittorrent/wiki/WebUI-API-(qBittorrent-4.1)
  qbtPasswordHash =
    ''@ByteArray(ARQ77eY1NUZaQsuDHbIMCA==:0WMRkYTUWVT9wVvdDtHAjU9b3b7uB8NR1Gur2hmQCvCDpm39Q+PsJRJPaCU51dEiz+dTzh8qbPsL8WkFljQYFQ==)'';
in
pkgs.testers.runNixOSTest {
  name = "tql-qbittorrent";

  nodes.machine = { config, lib, pkgs, ... }: {
    imports = [ tqlModule ];

    services.tql = {
      enable = true;
      package = tqlPackage;
      api.enable = true;
      readWritePaths = [ "/var/lib/tql" ];
      # Pass TQL_QBIT_PASSWORD into every tql unit.
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

    environment.systemPackages = [ pkgs.curl pkgs.jq ];
  };

  testScript = ''
    start_all()
    machine.wait_for_unit("qbittorrent.service")
    machine.wait_for_open_port(${toString qbtPort})
    machine.wait_for_unit("tql-api.service")
    machine.wait_for_open_port(8080)

    # Sanity: qbittorrent WebUI answers and login with the seeded credentials works.
    machine.succeed(
        "curl -fsS --cookie-jar /tmp/c.jar "
        "--data 'username=admin&password=adminadmin' "
        "http://127.0.0.1:${toString qbtPort}/api/v2/auth/login | grep -q Ok"
    )

    # tql doctor against the live qbittorrent — the login probe must succeed.
    # We invoke via systemd-run so the EnvironmentFile (TQL_QBIT_PASSWORD) is loaded.
    out = machine.succeed(
        "systemd-run --pipe --wait --quiet "
        "--unit=tql-doctor-test "
        "--property=EnvironmentFile=${pkgs.writeText "tql-env" ''
          TQL_QBIT_PASSWORD=adminadmin
        ''} "
        "tql doctor --config $(systemctl show -p Environment tql-api.service | sed -n 's/.*TQL_CONFIG=\\([^ ]*\\).*/\\1/p') --json"
    )
    print(out)
    import json
    payload = json.loads(out)
    by_name = {c["name"]: c for c in payload["checks"]}
    assert by_name["qbittorrent.login"]["status"] == "ok", by_name["qbittorrent.login"]
    assert by_name["qbittorrent.version"]["status"] == "ok", by_name["qbittorrent.version"]
  '';
}
