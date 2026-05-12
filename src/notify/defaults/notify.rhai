// Default tql notification shape() function.
//
// Reproduces the pre-Leg-58 hardcoded Telegram format byte-for-byte.
// HTML target emits bold/italic markup + warnings; non-HTML targets
// emit a flat "name [category]" line with link diffs and no warnings.
//
// Args:
//   fields: object map mirroring the spool Event.
//   escape: Rhai FnPtr; call it as `escape.call(s)` (Rhai's function-pointer
//           syntax — direct `escape(s)` would attempt a static dispatch).
//   target: "HTML" | "MarkdownV2" | "Plain"

fn shape(fields, escape, target) {
    let is_html = target == "HTML";
    fields.is_html = is_html;
    if is_html {
        fields.name_out = escape.call(fields.name);
        fields.category_out = escape.call(fields.category);
        fields.added_out = fields.link_sites_added.map(|s| escape.call(s));
        fields.removed_out = fields.link_sites_removed.map(|s| escape.call(s));
        fields.warnings_out = fields.warnings.map(|s| escape.call(s));
    } else {
        fields.name_out = fields.name;
        fields.category_out = fields.category;
        fields.added_out = fields.link_sites_added;
        fields.removed_out = fields.link_sites_removed;
        fields.warnings_out = [];
    }
    fields
}
