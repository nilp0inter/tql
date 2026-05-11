// MyAnonamouse classifier.
//
// Inputs come from the manifest (see manifest.toml). The host injects
// `canonical_category = "myanonamouse.net"` — this script must not return
// one. The post-processor appends sanitize(<torrent_name>) as the leaf of
// every link site, so the scraped <title> is part of the path, not the
// leaf.

fn classify(input) {
    let link_tags = [];
    let info_tags = [];
    let warnings = [];

    let title = sanitize(input.title);

    // main_cat IDs -> human names. Unknown IDs degrade gracefully so the
    // add still succeeds; the operator gets a warning and a stable fallback.
    let main_names = [];
    for id in input.main_cat {
        let name = if id == 13 { "AudioBooks" }
            else if id == 14 { "E-Books" }
            else if id == 15 { "Musicology" }
            else if id == 16 { "Radio" }
            else {
                warnings.push("unknown main_cat id: " + id);
                "main_" + id
            };
        main_names.push(name);
    }

    // Primary link sites: <main_cat>/<sub_category>/<title>.
    // sub-category names may themselves contain "/" (e.g. "Thriller/Suspense");
    // pass them through verbatim so each segment becomes its own directory
    // level. The host validator sanitizes each component before the tag is
    // accepted.
    for m in main_names {
        for sub in input.categories {
            link_tags.push("link:" + m + "/" + sub + "/" + title);
        }
    }

    // Author index.
    for a in input.authors {
        let an = sanitize(a);
        if an != "" {
            link_tags.push("link:_authors/" + an + "/" + title);
        }
    }

    // Series index.
    for s in input.series {
        let sn = sanitize(s);
        if sn != "" {
            link_tags.push("link:_series/" + sn + "/" + title);
        }
    }

    // Narrator index.
    for n in input.narrators {
        let nn = sanitize(n);
        if nn != "" {
            link_tags.push("link:_narrators/" + nn + "/" + title);
        }
    }

    // ISBN/Identifier index.
    if input.isbn != () && input.isbn != "" {
        link_tags.push("link:_isbn/" + sanitize(input.isbn) + "/" + title);
    }

    // Info tags.
    for label in input.labels {
        info_tags.push("label:" + label);
    }
    if input.primary_filetype != () && input.primary_filetype != "" {
        info_tags.push("filetype:" + input.primary_filetype);
    }
    if input.language != () && input.language != "" {
        info_tags.push("language:" + input.language);
    }

    if link_tags.is_empty() {
        warnings.push("no link tags produced");
    }

    #{ link_tags: link_tags, info_tags: info_tags, warnings: warnings }
}
