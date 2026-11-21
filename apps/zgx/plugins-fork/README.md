# Fork plugins

Our own zgx plugins, kept here as source. They replace desktop features the
fork used to carry as patches to the old GPUI app.

| Plugin | What it does | Needs the fork build? |
| --- | --- | --- |
| `clemann-usage-cards` | Plan usage cards at the foot of the sidebar for the device the chat in front runs on (`zeron.usage { device }`); needs the plugin, same version, allowed on that device too | 0.3.15.25+ to hide switched-off providers |
| `clemann-session-card` | The floating session card: device, project, branch `+N −M`, and the project's npm scripts, pinnable to the card | Yes: it calls `plugin.clemann-session-card.fork_card` |

The session card's bridge is `apps/zgx/src/plugins/fork_bridge.rs` (served
to that plugin id only), `apps/zgx/ui/screens/panel/fork_terminal.luau` (the
terminal tab) and the engine's `crates/engine/src/rpc/fork_session_card.rs`
(`ForkSessionCard`, and `RunProjectAction` for `npm:<script>` ids).

## Installing on a Mac

Each plugin's folder here is the signed copy (manifest, signature, the SDK
it was signed with), so Zeron can install it straight from GitHub. In
Settings → Plugins (choose the device at the top for another Mac), Install
from a link, and paste:

- `https://github.com/ClemannD/zeron/tree/main/apps/zgx/plugins-fork/clemann-usage-cards`
- `https://github.com/ClemannD/zeron/tree/main/apps/zgx/plugins-fork/clemann-session-card`

Allow it on that Mac when it asks. Zeron then offers updates from the same
link (Settings → Plugins, or `update_plugin`) whenever the version here is
higher and the same key signed it.

## Changing a plugin

Edit the installed copy on the Mac that signs them (`~/.zeron/plugins/<id>/`):
Zeron re-signs it on save. Bump `version` in its `zeron-plugin.json` for
other Macs to see an update, then copy the whole folder back here, exactly
(Zeron checks every file against the signature):

    rsync -a --delete --exclude '.*' ~/.zeron/plugins/<id>/ apps/zgx/plugins-fork/<id>/

The session card needs a fork build with the bridge (0.3.15.23 or later);
the usage cards need 0.3.15.24 or later to hide providers you've switched
off (older builds show every provider with usage).
