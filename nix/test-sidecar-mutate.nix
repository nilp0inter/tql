{ pkgs, tqlModule, tqlPackage }:

# End-to-end check for the mutating `tql sidecar` family:
# `tql sidecar repair` (filesystem mutator that rebuilds broken
# link sites) and `tql sidecar gc` (qBittorrent-coupled mutator
# that drops orphan sidecars + their hardlinks).
#
# Builds the same baseline as `test-sidecar.nix`: submit a
# `.torrent` via `tql cli`, synthesize a content file at
# qBittorrent's `content_path`, then `tql reconcile --json` to
# materialize one sidecar + two hardlinks.
#
# Then:
#   1. Break one hardlink, run `sidecar repair --dry-run --json`
#      (must not touch the filesystem), then `sidecar repair
#      --json` (must rebuild the link). Confirm with
#      `sidecar verify --json` that the world is clean again.
#   2. Delete the torrent from qBittorrent so the sidecar becomes
#      an orphan, run `sidecar gc --dry-run --json` (no
#      mutation), then `sidecar gc --json` (sidecar + sites
#      removed).

let
  qbtPort = 8085;
  qbtPasswordHash =
    ''@ByteArray(ARQ77eY1NUZaQsuDHbIMCA==:0WMRkYTUWVT9wVvdDtHAjU9b3b7uB8NR1Gur2hmQCvCDpm39Q+PsJRJPaCU51dEiz+dTzh8qbPsL8WkFljQYFQ==)'';

  exampleTracker = pkgs.runCommand "tql-tracker-example" { } ''
    mkdir -p $out
    cp -r ${../trackers/example}/. $out/
  '';
in
pkgs.testers.runNixOSTest {
  name = "tql-sidecar-mutate-e2e";

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

    machine.succeed("echo 'tql-sidecar-mutate-fixture' > /tmp/payload.txt")
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

    submit_out = machine.succeed(
        "systemd-run --pipe --wait --quiet --unit=tql-cli-scm "
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
    torrents = json.loads(info_raw)
    t = torrents[0]
    h = t["hash"]
    name = t["name"]
    save_path = t["save_path"].rstrip("/")
    content_path = t["content_path"]
    print("hash:", h, "name:", name, "content_path:", content_path)

    machine.succeed(f"install -d -o qbt -g qbt -m 0755 {save_path}")
    machine.succeed(
        f"install -o tql -g tql -m 0644 /tmp/payload.txt {content_path}"
    )

    rec_out = machine.succeed(
        "systemd-run --pipe --wait --quiet --unit=tql-rec-scm "
        "--uid=tql --gid=tql "
        f"--property=EnvironmentFile={env_file} "
        f"tql reconcile --json --config {cfg}"
    )
    report = json.loads(rec_out)
    assert report["summary"]["ok"] == 1, report

    sc_path = f"/var/lib/tql/library/.metadata/{h}.json"
    machine.succeed(f"test -f {sc_path}")

    link1 = f"/var/lib/tql/library/example.org/Books/Technical/Ada/{name}"
    link2 = f"/var/lib/tql/library/example.org/_authors/Ada/{name}"
    machine.succeed(f"test -e {link1!r}")
    machine.succeed(f"test -e {link2!r}")

    def run_tql(*args):
        cmd = " ".join(args)
        return machine.succeed(
            "systemd-run --pipe --wait --quiet "
            f"--unit=tql-scm-{abs(hash(cmd)) % 100000} "
            "--uid=tql --gid=tql "
            f"--property=EnvironmentFile={env_file} "
            f"tql {cmd}"
        )

    def run_tql_fail(*args):
        cmd = " ".join(args)
        rc, out = machine.execute(
            "systemd-run --pipe --wait --quiet "
            f"--unit=tql-scm-fail-{abs(hash(cmd)) % 100000} "
            "--uid=tql --gid=tql "
            f"--property=EnvironmentFile={env_file} "
            f"tql {cmd}"
        )
        return rc, out

    # ── sidecar repair ──────────────────────────────────────────
    # Break one hardlink (missing_resolved on _authors/Ada).
    machine.succeed(f"rm {link2!r}")

    # Verify reports the breakage and exits 1.
    rc, vbroken = run_tql_fail(f"sidecar verify --json --config {cfg}")
    assert rc != 0, (rc, vbroken)
    rep_v = json.loads(vbroken)
    assert rep_v["summary"]["with_issues"] == 1, rep_v

    # Dry-run must not mutate the filesystem.
    dry_out = run_tql(
        f"sidecar repair --json --dry-run --config {cfg}"
    )
    print("repair dry:", dry_out)
    dry = json.loads(dry_out)
    assert dry["dry_run"] is True, dry
    s_dry = dry["summary"]
    assert s_dry["scanned"] == 1, s_dry
    assert s_dry["planned"] >= 1, s_dry
    assert s_dry["repaired"] == 0, s_dry
    assert s_dry["failed"] == 0, s_dry
    assert any(
        a["info_hash_v1"] == h
        and a["site"] == "_authors/Ada"
        and a["action"] == "relink"
        and a["outcome"] == "planned"
        for a in dry["actions"]
    ), dry["actions"]
    machine.fail(f"test -e {link2!r}")

    # Apply repair for real.
    repair_out = run_tql(f"sidecar repair --json --config {cfg}")
    print("repair apply:", repair_out)
    rep_r = json.loads(repair_out)
    assert rep_r["dry_run"] is False, rep_r
    s_r = rep_r["summary"]
    assert s_r["scanned"] == 1, s_r
    assert s_r["repaired"] >= 1, s_r
    assert s_r["failed"] == 0, s_r
    assert any(
        a["info_hash_v1"] == h
        and a["site"] == "_authors/Ada"
        and a["action"] == "relink"
        and a["outcome"] == "repaired"
        for a in rep_r["actions"]
    ), rep_r["actions"]
    machine.succeed(f"test -e {link2!r}")

    # Sidecar verify must be clean post-repair.
    v_clean = json.loads(
        run_tql(f"sidecar verify --json --config {cfg}")
    )
    s_clean = v_clean["summary"]
    assert s_clean["with_issues"] == 0, s_clean
    assert s_clean["issues_total"] == 0, s_clean

    # ── sidecar gc ──────────────────────────────────────────────
    # Delete the torrent from qBittorrent (deleteFiles=false:
    # leaves the seed file in place, just drops the entry from
    # qBit's known set). The sidecar is now an orphan.
    machine.succeed(
        "curl -fsS --cookie /tmp/c.jar "
        "--data 'hashes=" + h + "&deleteFiles=false' "
        "http://127.0.0.1:${toString qbtPort}/api/v2/torrents/delete"
    )
    machine.wait_until_succeeds(
        "curl -fsS --cookie /tmp/c.jar "
        "http://127.0.0.1:${toString qbtPort}/api/v2/torrents/info "
        "| jq -e 'length == 0'",
        timeout=15,
    )

    # Dry-run: orphan visible, nothing removed yet.
    gc_dry_out = run_tql(
        f"sidecar gc --json --dry-run --config {cfg}"
    )
    print("gc dry:", gc_dry_out)
    gc_dry = json.loads(gc_dry_out)
    assert gc_dry["dry_run"] is True, gc_dry
    sg_dry = gc_dry["summary"]
    assert sg_dry["scanned"] == 1, sg_dry
    assert sg_dry["orphans"] == 1, sg_dry
    assert sg_dry["removed"] == 0, sg_dry
    # dry-run reports the *planned* site unlinks (2) without mutating
    # the filesystem — the real check is that link1/link2 still exist
    # below.
    assert any(
        e["info_hash_v1"] == h and e["status"] == "orphan" and e["removed"] is False
        for e in gc_dry["entries"]
    ), gc_dry["entries"]
    machine.succeed(f"test -f {sc_path}")
    machine.succeed(f"test -e {link1!r}")
    machine.succeed(f"test -e {link2!r}")

    # Apply gc for real.
    gc_out = run_tql(f"sidecar gc --json --config {cfg}")
    print("gc apply:", gc_out)
    gc = json.loads(gc_out)
    assert gc["dry_run"] is False, gc
    sg = gc["summary"]
    assert sg["scanned"] == 1, sg
    assert sg["orphans"] == 1, sg
    assert sg["removed"] == 1, sg
    assert sg["sites_unlinked"] == 2, sg
    assert sg["errors"] == 0, sg
    assert any(
        e["info_hash_v1"] == h
        and e["status"] == "orphan"
        and e["removed"] is True
        and e["sites_unlinked"] == 2
        for e in gc["entries"]
    ), gc["entries"]

    machine.fail(f"test -e {sc_path}")
    machine.fail(f"test -e {link1!r}")
    machine.fail(f"test -e {link2!r}")

    # Second gc run is a no-op: nothing to scan.
    gc2 = json.loads(run_tql(f"sidecar gc --json --config {cfg}"))
    sg2 = gc2["summary"]
    assert sg2["scanned"] == 0, sg2
    assert sg2["orphans"] == 0, sg2
    assert sg2["removed"] == 0, sg2
  '';
}
