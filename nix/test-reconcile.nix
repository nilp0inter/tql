{ pkgs, tqlModule, tqlPackage }:

# End-to-end check for `tql reconcile` against a real qbittorrent-nox.
#
# Same setup shape as `test-post-process.nix` but skips the explicit
# post-process invocation: reconcile is the sole producer of the
# sidecar and hardlinks. This proves the periodic-safety-net
# subcommand (DESIGN.md §7) materializes the same on-disk state the
# torrent-finished hook would have.
#
# Steps:
#   1. Submit a `.torrent` via `tql cli` so qBittorrent has a torrent
#      whose tags reflect the classifier output.
#   2. Materialize a synthetic content file at qBittorrent's
#      `content_path` (qBittorrent itself never downloads because the
#      tracker is bogus).
#   3. Run `tql reconcile --json` and assert: total=1, ok=1,
#      aborted=0, the entry status is "ok", sidecar/hardlinks match
#      the classifier's link_tags.
#   4. Re-run reconcile to confirm idempotence in the cross-process
#      flow (no warnings, sidecar unchanged).

let
  qbtPort = 8084;
  qbtPasswordHash =
    ''@ByteArray(ARQ77eY1NUZaQsuDHbIMCA==:0WMRkYTUWVT9wVvdDtHAjU9b3b7uB8NR1Gur2hmQCvCDpm39Q+PsJRJPaCU51dEiz+dTzh8qbPsL8WkFljQYFQ==)'';

  exampleTracker = pkgs.runCommand "tql-tracker-example" { } ''
    mkdir -p $out
    cp -r ${../trackers/example}/. $out/
  '';
in
pkgs.testers.runNixOSTest {
  name = "tql-reconcile-e2e";

  nodes.machine = { config, lib, pkgs, ... }: {
    imports = [ tqlModule ];

    services.tql = {
      enable = true;
      package = tqlPackage;
      api.enable = true;
      readWritePaths = [ "/var/lib/tql" "/var/lib/downloads" ];
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
        linking = {
          prefer = "hardlink";
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

      [BitTorrent]
      Session\DefaultSavePath=/var/lib/downloads/
    '';

    systemd.tmpfiles.rules = [
      "d /var/lib/tql/seed                       0755 tql tql -"
      "d /var/lib/tql/library                    0755 tql tql -"
      "d /var/lib/tql/trackers                   0755 tql tql -"
      "L+ /var/lib/tql/trackers/example          -    -   -   - ${exampleTracker}"
      "d /var/lib/qbt/qBittorrent                0755 qbt qbt -"
      "d /var/lib/qbt/qBittorrent/config         0755 qbt qbt -"
      "d /var/lib/downloads                      0755 qbt qbt -"
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

    environment.systemPackages = [ pkgs.curl pkgs.jq pkgs.mktorrent ];
  };

  testScript = ''
    import json

    start_all()
    machine.wait_for_unit("qbittorrent.service")
    machine.wait_for_open_port(${toString qbtPort})
    machine.wait_for_unit("tql-api.service")
    machine.wait_for_open_port(8080)

    machine.succeed("test -f /var/lib/tql/trackers/example/manifest.toml")

    machine.succeed("echo 'tql-reconcile-fixture' > /tmp/payload.txt")
    machine.succeed(
        "mktorrent -a 'http://tracker.invalid/announce' "
        "-o /tmp/sample.torrent /tmp/payload.txt"
    )

    cfg = machine.succeed(
        "systemctl show -p Environment tql-api.service "
        "| sed -n 's/.*TQL_CONFIG=\\([^ ]*\\).*/\\1/p'"
    ).strip()
    print("config:", cfg)

    env_file = "${pkgs.writeText "tql-env" ''
      TQL_QBIT_PASSWORD=adminadmin
    ''}"

    # Submit via tql cli so qBittorrent's tags reflect classifier output.
    submit_out = machine.succeed(
        "systemd-run --pipe --wait --quiet --unit=tql-cli-rec "
        f"--property=EnvironmentFile={env_file} "
        f"tql cli --config {cfg} example "
        "--url=https://example.org/t/123 "
        "--categories=Books/Technical "
        "--author=Ada "
        "/tmp/sample.torrent"
    )
    ack = json.loads(submit_out)
    assert ack["ok"] is True, ack
    assert ack["category"] == "example.org", ack

    # Read torrent metadata back.
    machine.succeed(
        "curl -fsS --cookie-jar /tmp/c.jar "
        "--data 'username=admin&password=adminadmin' "
        "http://127.0.0.1:${toString qbtPort}/api/v2/auth/login | grep -q Ok"
    )
    machine.wait_until_succeeds(
        "curl -fsS --cookie /tmp/c.jar "
        "http://127.0.0.1:${toString qbtPort}/api/v2/torrents/info "
        "| jq -e 'length >= 1'",
        timeout=30,
    )
    info_raw = machine.succeed(
        "curl -fsS --cookie /tmp/c.jar "
        "http://127.0.0.1:${toString qbtPort}/api/v2/torrents/info"
    )
    print("qbt info:", info_raw)
    torrents = json.loads(info_raw)
    t = torrents[0]
    h = t["hash"]
    name = t["name"]
    save_path = t["save_path"].rstrip("/")
    content_path = t["content_path"]
    tags = t["tags"]
    category = t["category"]
    print("hash:", h, "name:", name, "content_path:", content_path)
    assert category == "example.org", t
    assert "link:Books/Technical/Ada" in tags
    assert "link:_authors/Ada" in tags

    # Materialize the on-disk file that a real download would have left.
    # Owned by tql so fs.protected_hardlinks doesn't block link(2).
    machine.succeed(f"install -d -o qbt -g qbt -m 0755 {save_path}")
    machine.succeed(
        f"install -o tql -g tql -m 0644 /tmp/payload.txt {content_path}"
    )

    sc_path = f"/var/lib/tql/library/.metadata/{h}.json"
    machine.succeed(f"test ! -e {sc_path}")  # reconcile is the only producer

    # Run reconcile (NOT post-process) and parse JSON.
    rec_out = machine.succeed(
        "systemd-run --pipe --wait --quiet --unit=tql-rec "
        "--uid=tql --gid=tql "
        f"--property=EnvironmentFile={env_file} "
        f"tql reconcile --json --config {cfg}"
    )
    print("reconcile out:", rec_out)
    report = json.loads(rec_out)
    assert report["dry_run"] is False, report
    s = report["summary"]
    assert s["total"] == 1 and s["ok"] == 1 and s["aborted"] == 0, s
    entries = report["entries"]
    assert len(entries) == 1, entries
    e = entries[0]
    assert e["info_hash_v1"] == h, e
    assert e["status"] == "ok", e
    assert e.get("warnings", []) == [], e

    # Sidecar materialized by reconcile.
    sc_raw = machine.succeed(f"cat {sc_path}")
    sidecar = json.loads(sc_raw)
    assert sidecar["info_hash_v1"] == h, sidecar
    assert sidecar["category"] == "example.org", sidecar
    assert sidecar["name"] == name, sidecar
    rels = sorted(ls["relative_path"] for ls in sidecar["link_sites"])
    assert "Books/Technical/Ada" in rels, rels
    assert "_authors/Ada" in rels, rels
    assert sidecar.get("warnings", []) == [], sidecar

    # Hardlinks present and share an inode with the source.
    link1 = f"/var/lib/tql/library/example.org/Books/Technical/Ada/{name}"
    link2 = f"/var/lib/tql/library/example.org/_authors/Ada/{name}"
    src_ino = machine.succeed(f"stat -c %i {content_path!r}").strip()
    l1_ino = machine.succeed(f"stat -c %i {link1!r}").strip()
    l2_ino = machine.succeed(f"stat -c %i {link2!r}").strip()
    assert src_ino == l1_ino == l2_ino, (src_ino, l1_ino, l2_ino)

    # Idempotence: rerun reconcile, expect same outcome.
    rec_out2 = machine.succeed(
        "systemd-run --pipe --wait --quiet --unit=tql-rec-rerun "
        "--uid=tql --gid=tql "
        f"--property=EnvironmentFile={env_file} "
        f"tql reconcile --json --config {cfg}"
    )
    report2 = json.loads(rec_out2)
    s2 = report2["summary"]
    assert s2["total"] == 1 and s2["ok"] == 1 and s2["aborted"] == 0, s2
    e2 = report2["entries"][0]
    assert e2["status"] == "ok", e2
    assert e2.get("adds", []) == [], e2
    assert e2.get("removes", []) == [], e2
    assert e2.get("warnings", []) == [], e2

    sidecar_after = json.loads(machine.succeed(f"cat {sc_path}"))
    rels_after = sorted(ls["relative_path"] for ls in sidecar_after["link_sites"])
    assert rels == rels_after, (rels, rels_after)

    # Dry-run on a converged state should be a no-op too.
    dry_out = machine.succeed(
        "systemd-run --pipe --wait --quiet --unit=tql-rec-dry "
        "--uid=tql --gid=tql "
        f"--property=EnvironmentFile={env_file} "
        f"tql reconcile --dry-run --json --config {cfg}"
    )
    dry = json.loads(dry_out)
    assert dry["dry_run"] is True, dry
    sd = dry["summary"]
    assert sd["total"] == 1 and sd["aborted"] == 0, sd
    de = dry["entries"][0]
    assert de.get("adds", []) == [], de
    assert de.get("removes", []) == [], de
  '';
}
