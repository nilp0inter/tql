// Minimal example classifier. Produces one link tag per category, plus an
// optional "_authors/<author>" facet, and turns labels into info tags.

fn classify(input) {
    let link_tags = [];
    let info_tags = [];
    let warnings = [];

    // Categories already use "/" as a structural separator (e.g.
    // "Books/Technical"); we trust them verbatim. Only the author (a
    // free-form field) gets sanitized to one safe component.
    for cat in input.categories {
        if input.author != () && input.author != "" {
            link_tags.push("link:" + cat + "/" + sanitize(input.author));
        } else {
            link_tags.push("link:" + cat);
        }
    }

    if input.author != () && input.author != "" {
        link_tags.push("link:_authors/" + sanitize(input.author));
    }

    if input.labels != () {
        for label in input.labels {
            info_tags.push("label:" + label);
        }
    }

    #{ link_tags: link_tags, info_tags: info_tags, warnings: warnings }
}
