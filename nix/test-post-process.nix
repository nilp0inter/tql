{ pkgs, tqlModule, tqlPackage }:

# End-to-end check for the post-process pipeline.
#
# Boots qbittorrent-nox + tql in a NixOS VM, submits a real `.torrent` via
# `tql cli` (same as `test-cli.nix`), then:
#   1. Reads the torrent metadata back from qBittorrent's WebUI
#      (hash, name, save_path, content_path, tags, category, size).
#   2. Materializes a fake "downloaded" payload at qBittorrent's reported
#      content_path — qBittorrent will never actually complete this torrent
#      because the tracker URL is bogus, so we synthesize the on-disk state
#      that a real download would have left behind.
#   3. Invokes `tql post-process --hash … --tags … …` with exactly the
#      arguments qBittorrent's "torrent finished" hook would have passed.
#   4. Asserts the sidecar JSON under <library_root>/.metadata/<hash>.json
#      and the hardlinks under <library_root>/<category>/<rel>/<name>
#      match the classifier's link_tags.
#
# This is the natural follow-up to `test-cli.nix`, completing the cli →
# qbittorrent → post-process → sidecar/library e2e loop.

let
  qbtPort = 8082;
  qbtPasswordHash =
    ''@ByteArray(ARQ77eY1NUZaQsuDHbIMCA==:0WMRkYTUWVT9wVvdDtHAjU9b3b7uB8NR1Gur2hmQCvCDpm39Q+PsJRJPaCU51dEiz+dTzh8qbPsL8WkFljQYFQ==)'';

  exampleTracker = pkgs.runCommand "tql-tracker-example" { } ''
    mkdir -p $out
    cp -r ${../trackers/example}/. $out/
  '';
in
pkgs.testers.runNixOSTest {
  name = "tql-post-process-e2e";

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

    # Pin qBittorrent's default save path so we know where to drop the
    # synthetic content file. Without this it defaults to
    # ~/Downloads = /var/lib/qbt/Downloads, but pinning it explicitly
    # protects the test against future qBittorrent default changes.
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

    # Sanity: tracker bundle present.
    machine.succeed("test -f /var/lib/tql/trackers/example/manifest.toml")

    # Build a tiny .torrent and submit it via `tql cli`.
    machine.succeed("echo 'tql-pp-fixture' > /tmp/payload.txt")
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
        "systemd-run --pipe --wait --quiet --unit=tql-cli-pp "
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

    # Authenticate with qBittorrent and read the torrent's full metadata.
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
    assert len(torrents) >= 1, torrents
    t = torrents[0]
    h = t["hash"]
    name = t["name"]
    save_path = t["save_path"].rstrip("/")
    content_path = t["content_path"]
    tags = t["tags"]
    category = t["category"]
    size = int(t["size"])
    # qBittorrent reports size=0 until it has actually inspected the data,
    # which won't happen for a torrent with a bogus tracker. Fall back to
    # the synthetic payload size — post-process only stores `size`, never
    # validates it against on-disk bytes.
    if size == 0:
        size = len("tql-pp-fixture\n")
    print("hash:", h, "name:", name, "save_path:", save_path,
          "content_path:", content_path, "tags:", tags)
    assert category == "example.org", t
    assert "link:Books/Technical/Ada" in tags
    assert "link:_authors/Ada" in tags

    # Materialize a synthetic "downloaded" file at content_path. qBittorrent
    # itself will never produce one (the tracker is invalid), so we stand in
    # for the part of the system that would normally write the bytes.
    #
    # We chown to tql:tql because Linux's fs.protected_hardlinks sysctl
    # (enabled by default) refuses link(2) when the caller doesn't own the
    # source. In a real deployment qbittorrent and tql post-process either
    # run under the same user, or the operator unsets that sysctl; for the
    # test we just write the file as the user that's about to hardlink it.
    machine.succeed(f"install -d -o qbt -g qbt -m 0755 {save_path}")
    machine.succeed(
        f"install -o tql -g tql -m 0644 /tmp/payload.txt {content_path}"
    )

    # Invoke post-process under the tql user (matching how the qBittorrent
    # "torrent finished" hook would invoke it under a real deployment).
    pp_out = machine.succeed(
        "systemd-run --pipe --wait --quiet --unit=tql-pp "
        "--uid=tql --gid=tql "
        f"--property=EnvironmentFile={env_file} "
        f"tql post-process --config {cfg} "
        f"--hash {h} "
        f"--name {name!r} "
        f"--category {category!r} "
        f"--tags {tags!r} "
        f"--content-path {content_path!r} "
        f"--save-path {save_path!r} "
        f"--size {size}"
    )
    print("post-process out:", pp_out)

    # Assert the sidecar exists with the expected shape.
    sc_path = f"/var/lib/tql/library/.metadata/{h}.json"
    sc_raw = machine.succeed(f"cat {sc_path}")
    print("sidecar:", sc_raw)
    sidecar = json.loads(sc_raw)
    assert sidecar["info_hash_v1"] == h, sidecar
    assert sidecar["category"] == "example.org", sidecar
    assert sidecar["name"] == name, sidecar
    assert sidecar["is_directory"] is False, sidecar
    rels = sorted(ls["relative_path"] for ls in sidecar["link_sites"])
    assert "Books/Technical/Ada" in rels, rels
    assert "_authors/Ada" in rels, rels
    assert sidecar.get("warnings", []) == [], sidecar

    # Assert the hardlinks exist and share an inode with the source file.
    link1 = f"/var/lib/tql/library/example.org/Books/Technical/Ada/{name}"
    link2 = f"/var/lib/tql/library/example.org/_authors/Ada/{name}"
    machine.succeed(f"test -f {link1!r}")
    machine.succeed(f"test -f {link2!r}")
    src_ino = machine.succeed(f"stat -c %i {content_path!r}").strip()
    l1_ino = machine.succeed(f"stat -c %i {link1!r}").strip()
    l2_ino = machine.succeed(f"stat -c %i {link2!r}").strip()
    assert src_ino == l1_ino == l2_ino, (src_ino, l1_ino, l2_ino)

    # Re-run post-process: must be idempotent (no warnings, sidecar unchanged
    # in shape, hardlinks still present).
    machine.succeed(
        "systemd-run --pipe --wait --quiet --unit=tql-pp-rerun "
        "--uid=tql --gid=tql "
        f"--property=EnvironmentFile={env_file} "
        f"tql post-process --config {cfg} "
        f"--hash {h} --name {name!r} --category {category!r} "
        f"--tags {tags!r} --content-path {content_path!r} "
        f"--save-path {save_path!r} --size {size}"
    )
    sc_raw2 = machine.succeed(f"cat {sc_path}")
    sidecar2 = json.loads(sc_raw2)
    rels2 = sorted(ls["relative_path"] for ls in sidecar2["link_sites"])
    assert rels == rels2, (rels, rels2)
    assert sidecar2.get("warnings", []) == [], sidecar2
    machine.succeed(f"test -f {link1!r}")
    machine.succeed(f"test -f {link2!r}")
  '';
}
