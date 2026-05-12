// CGPeers classifier.
//
// Inputs come from the manifest (see manifest.toml). The host injects
// `canonical_category = "cgpeers.to"` — this script must not return one.
// The post-processor appends sanitize(<torrent_name>) as the leaf of
// every link site, so the scraped <title> is part of the path, not the
// leaf.

fn classify(input) {
    let link_tags = [];
    let info_tags = [];
    let warnings = [];

    let title = sanitize(input.title);

    // Primary link site: <main_cat>/<title>. main_cat is the human name
    // scraped from the cats_icon div's title attribute.
    link_tags.push("link:" + input.main_cat + "/" + title);

    // Tag index. Tags are community-voted facets and the main way
    // operators find CGPeers content; mirror each as its own link site.
    for t in input.tags {
        let tn = sanitize(t);
        if tn != "" {
            link_tags.push("link:_tags/" + tn + "/" + title);
        }
    }

    // Uploader index.
    if input.uploader != () && input.uploader != "" {
        let un = sanitize(input.uploader);
        if un != "" {
            link_tags.push("link:_uploaders/" + un + "/" + title);
        }
    }

    // Info tags.
    for label in input.labels {
        info_tags.push("label:" + label);
    }
    for t in input.tags {
        info_tags.push("tag:" + t);
    }

    if link_tags.is_empty() {
        warnings.push("no link tags produced");
    }

    #{ link_tags: link_tags, info_tags: info_tags, warnings: warnings }
}
