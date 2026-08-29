{ pkgs, tqlModule, tqlPackage }:

# End-to-end check for `tql link add` / `tql link remove` against a
# real qbittorrent-nox.
#
# Setup mirrors `test-reconcile.nix`:
#   1. Submit a `.torrent` via `tql cli` so qBittorrent has a torrent
#      whose tags reflect the classifier output.
#   2. Materialize a synthetic content file at qBittorrent's
#      `content_path` (the tracker URL is bogus, so qBittorrent never
#      downloads on its own).
#   3. Run `tql reconcile` once to build the baseline sidecar +
#      hardlinks (two link sites from the classifier).
#   4. `tql link add <hash> Books/Custom --json` — appends a new link
#      tag to qBittorrent and runs the post-process pipeline so a
#      third hardlink site materializes. Asserts:
#        - ack JSON status="ok"
#        - sidecar gains the new relative_path
#        - new hardlink at <library>/<cat>/Books/Custom/<name> shares
#          an inode with the source
#        - qBittorrent's tag list now contains link:Books/Custom
#   5. Re-run `tql link add` with the same path — idempotent (status
#      ok, sidecar unchanged).
#   6. `tql link remove <hash> _authors/Ada --json` — strips the
#      classifier-generated tag and prunes the link site. Asserts:
#        - ack JSON status="ok"
#        - sidecar loses the _authors/Ada relative_path
#        - the directory <library>/<cat>/_authors is pruned
#        - qBittorrent's tag list no longer contains link:_authors/Ada

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
  name = "tql-link-e2e";

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

    machine.succeed("echo 'tql-link-fixture' > /tmp/payload.txt")
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

    # Submit via tql cli so classifier-generated tags land on the torrent.
    submit_out = machine.succeed(
        "systemd-run --pipe --wait --quiet --unit=tql-cli-link "
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
    tags = t["tags"]
    category = t["category"]
    print("hash:", h, "name:", name, "content_path:", content_path)
    assert category == "example.org", t
    assert "link:Books/Technical/Ada" in tags
    assert "link:_authors/Ada" in tags

    # Materialize the on-disk file; owned by tql so the hardlinker can link(2).
    machine.succeed(f"install -d -o qbt -g qbt -m 0755 {save_path}")
    machine.succeed(
        f"install -o tql -g tql -m 0644 /tmp/payload.txt {content_path}"
    )

    # Baseline sidecar + 2 hardlinks via reconcile.
    machine.succeed(
        "systemd-run --pipe --wait --quiet --unit=tql-rec-baseline "
        "--uid=tql --gid=tql "
        f"--property=EnvironmentFile={env_file} "
        f"tql reconcile --json --config {cfg}"
    )

    sc_path = f"/var/lib/tql/library/.metadata/{h}.json"
    sidecar = json.loads(machine.succeed(f"cat {sc_path}"))
    rels = sorted(ls["relative_path"] for ls in sidecar["link_sites"])
    assert rels == ["Books/Technical/Ada", "_authors/Ada"], rels

    link_base = "/var/lib/tql/library/example.org"
    src_ino = machine.succeed(f"stat -c %i {content_path!r}").strip()
    for rel in rels:
        ino = machine.succeed(f"stat -c %i {link_base}/{rel}/{name!r}").strip()
        assert ino == src_ino, (rel, ino, src_ino)

    # --- link add Books/Custom -------------------------------------
    add_out = machine.succeed(
        "systemd-run --pipe --wait --quiet --unit=tql-link-add "
        "--uid=tql --gid=tql "
        f"--property=EnvironmentFile={env_file} "
        f"tql link add {h} Books/Custom --config {cfg} --json"
    )
    print("link add out:", add_out)
    add_ack = json.loads(add_out)
    assert add_ack["op"] == "add", add_ack
    assert add_ack["status"] == "ok", add_ack
    assert add_ack["hash"] == h, add_ack
    assert add_ack["path"] == "Books/Custom", add_ack

    sidecar2 = json.loads(machine.succeed(f"cat {sc_path}"))
    rels2 = sorted(ls["relative_path"] for ls in sidecar2["link_sites"])
    assert rels2 == ["Books/Custom", "Books/Technical/Ada", "_authors/Ada"], rels2

    new_link = f"{link_base}/Books/Custom/{name}"
    new_ino = machine.succeed(f"stat -c %i {new_link!r}").strip()
    assert new_ino == src_ino, (new_ino, src_ino)

    # qBittorrent reflects the new tag.
    info_raw2 = machine.succeed(
        "curl -fsS --cookie /tmp/c.jar "
        f"http://127.0.0.1:${toString qbtPort}/api/v2/torrents/info?hashes={h}"
    )
    tags_after_add = json.loads(info_raw2)[0]["tags"]
    assert "link:Books/Custom" in tags_after_add, tags_after_add

    # --- link add (same path) is idempotent ------------------------
    add_out2 = machine.succeed(
        "systemd-run --pipe --wait --quiet --unit=tql-link-add-rerun "
        "--uid=tql --gid=tql "
        f"--property=EnvironmentFile={env_file} "
        f"tql link add {h} Books/Custom --config {cfg} --json"
    )
    add_ack2 = json.loads(add_out2)
    assert add_ack2["status"] == "ok", add_ack2
    sidecar3 = json.loads(machine.succeed(f"cat {sc_path}"))
    rels3 = sorted(ls["relative_path"] for ls in sidecar3["link_sites"])
    assert rels3 == rels2, (rels3, rels2)

    # --- link remove _authors/Ada ----------------------------------
    rm_out = machine.succeed(
        "systemd-run --pipe --wait --quiet --unit=tql-link-rm "
        "--uid=tql --gid=tql "
        f"--property=EnvironmentFile={env_file} "
        f"tql link remove {h} _authors/Ada --config {cfg} --json"
    )
    print("link remove out:", rm_out)
    rm_ack = json.loads(rm_out)
    assert rm_ack["op"] == "remove", rm_ack
    assert rm_ack["status"] == "ok", rm_ack
    assert rm_ack["path"] == "_authors/Ada", rm_ack

    sidecar4 = json.loads(machine.succeed(f"cat {sc_path}"))
    rels4 = sorted(ls["relative_path"] for ls in sidecar4["link_sites"])
    assert rels4 == ["Books/Custom", "Books/Technical/Ada"], rels4

    # The pruned link directory should be gone (linking.rs prunes
    # newly-empty parents up to <library_root>/<category>/).
    machine.succeed(f"test ! -e {link_base}/_authors")

    info_raw3 = machine.succeed(
        "curl -fsS --cookie /tmp/c.jar "
        f"http://127.0.0.1:${toString qbtPort}/api/v2/torrents/info?hashes={h}"
    )
    tags_after_rm = json.loads(info_raw3)[0]["tags"]
    assert "link:_authors/Ada" not in tags_after_rm, tags_after_rm
    assert "link:Books/Custom" in tags_after_rm, tags_after_rm
    assert "link:Books/Technical/Ada" in tags_after_rm, tags_after_rm
  '';
}
