{ pkgs, tqlModule, tqlPackage }:

# End-to-end check for the read-only `tql sidecar` family
# (`list`, `show`, `verify`) against a sidecar materialized by a
# real `tql reconcile` run on top of qbittorrent-nox.
#
# Steps:
#   1. Submit a `.torrent` via `tql cli` so qBittorrent has a
#      torrent whose tags reflect the classifier output.
#   2. Materialize a synthetic content file at qBittorrent's
#      `content_path` (qBittorrent itself never downloads — the
#      tracker is bogus).
#   3. Run `tql reconcile --json` to build the sidecar + hardlinks.
#   4. Exercise `sidecar list` (plain, --json, --category filter),
#      `sidecar show <hash>` (existing + unknown-hash error),
#      and `sidecar verify --json` (clean state).
#   5. Break a hardlink, rerun `sidecar verify --json`, assert it
#      reports `missing_resolved` and exits 1.

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
  name = "tql-sidecar-e2e";

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

    machine.succeed("echo 'tql-sidecar-fixture' > /tmp/payload.txt")
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
        "systemd-run --pipe --wait --quiet --unit=tql-cli-sc "
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
        "status=$(curl -fsS -o /tmp/qbt-login-body -w '%{http_code}' --cookie-jar /tmp/c.jar "
        "--data 'username=admin&password=adminadmin' "
        "http://127.0.0.1:${toString qbtPort}/api/v2/auth/login); "
        "if test \"$status\" = 204; then "
        "test ! -s /tmp/qbt-login-body; "
        "else "
        "test \"$status\" = 200 && test \"$(cat /tmp/qbt-login-body)\" = 'Ok.'; "
        "fi"
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

    # Build the baseline sidecar + hardlinks via reconcile.
    rec_out = machine.succeed(
        "systemd-run --pipe --wait --quiet --unit=tql-rec-sc "
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
            f"--unit=tql-sc-{abs(hash(cmd)) % 100000} "
            "--uid=tql --gid=tql "
            f"--property=EnvironmentFile={env_file} "
            f"tql {cmd}"
        )

    def run_tql_fail(*args):
        cmd = " ".join(args)
        rc, out = machine.execute(
            "systemd-run --pipe --wait --quiet "
            f"--unit=tql-sc-fail-{abs(hash(cmd)) % 100000} "
            "--uid=tql --gid=tql "
            f"--property=EnvironmentFile={env_file} "
            f"tql {cmd}"
        )
        return rc, out

    # ── sidecar list ────────────────────────────────────────────
    list_out = run_tql(f"sidecar list --json --config {cfg}")
    print("list out:", list_out)
    items = json.loads(list_out)
    assert isinstance(items, list) and len(items) == 1, items
    li = items[0]
    assert li["info_hash_v1"] == h, li
    assert li["category"] == "example.org", li
    assert li["name"] == name, li
    assert li["sites_count"] == 2, li
    assert li["is_directory"] is False, li

    # --category filter (matching).
    list_match = json.loads(
        run_tql(f"sidecar list --json --category example.org --config {cfg}")
    )
    assert len(list_match) == 1 and list_match[0]["info_hash_v1"] == h, list_match

    # --category filter (non-matching) → empty array.
    list_miss = json.loads(
        run_tql(f"sidecar list --json --category nope.invalid --config {cfg}")
    )
    assert list_miss == [], list_miss

    # ── sidecar show ────────────────────────────────────────────
    show_out = run_tql(f"sidecar show {h} --config {cfg}")
    sidecar = json.loads(show_out)
    assert sidecar["info_hash_v1"] == h, sidecar
    assert sidecar["category"] == "example.org", sidecar
    rels = sorted(ls["relative_path"] for ls in sidecar["link_sites"])
    assert "Books/Technical/Ada" in rels, rels
    assert "_authors/Ada" in rels, rels

    # show with an unknown hash → non-zero exit.
    bogus = "0" * 40
    rc, out = run_tql_fail(f"sidecar show {bogus} --config {cfg}")
    assert rc != 0, (rc, out)

    # ── sidecar verify (clean) ──────────────────────────────────
    verify_out = run_tql(f"sidecar verify --json --config {cfg}")
    print("verify clean:", verify_out)
    rep = json.loads(verify_out)
    s = rep["summary"]
    assert s["scanned"] == 1, s
    assert s["ok"] == 1, s
    assert s["with_issues"] == 0, s
    assert s["issues_total"] == 0, s
    e = rep["entries"][0]
    assert e["info_hash_v1"] == h, e
    assert e["ok"] is True, e
    assert e["issues"] == [], e

    # ── sidecar verify (broken hardlink) ────────────────────────
    machine.succeed(f"rm {link2!r}")
    rc, broken_out = run_tql_fail(f"sidecar verify --json --config {cfg}")
    print("verify broken:", broken_out)
    assert rc != 0, (rc, broken_out)
    rep_b = json.loads(broken_out)
    sb = rep_b["summary"]
    assert sb["scanned"] == 1, sb
    assert sb["with_issues"] == 1, sb
    assert sb["issues_total"] >= 1, sb
    eb = rep_b["entries"][0]
    assert eb["info_hash_v1"] == h, eb
    assert eb["ok"] is False, eb
    issues = eb["issues"]
    assert any(
        iss["kind"] == "missing_resolved"
        and iss["site"] == "_authors/Ada"
        and iss["path"].endswith(name)
        for iss in issues
    ), issues
  '';
}
