// The user's choice of formats (`slerp associate`, the viewer's Settings >
// File Types), so the plugin leaves alone the formats whose thumbnails are
// switched off, as the file manager's thumbnailer does.

#pragma once

namespace slerp {

/**
 * Whether the user switched off Slerp's thumbnails for the format with this
 * id (`file-types.jsonc` in `$XDG_CONFIG_HOME/slerp`, else
 * `~/.config/slerp`).  The file is read again only when it changes, so
 * asking costs a `stat`.  Formats the user never mentioned keep the
 * table's default, which is on for every format the plugin claims. */
bool switchedOff(const char *formatId);

}
