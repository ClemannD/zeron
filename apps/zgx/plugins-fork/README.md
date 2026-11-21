# Fork plugins

Our own zgx plugins, kept here as source. They replace desktop features the
fork used to carry as patches to the old GPUI app.

| Plugin | What it does | Needs the fork build? |
| --- | --- | --- |
| `clemann-usage-cards` | Plan usage cards at the foot of the sidebar (`zeron.usage`) | No: plain plugin API |
| `clemann-session-card` | The floating session card: device, project, branch `+N −M`, and the project's npm scripts, pinnable to the card | Yes: it calls `plugin.clemann-session-card.fork_card` |

The session card's bridge is `apps/zgx/src/plugins/fork_bridge.rs` (served
to that plugin id only), `apps/zgx/ui/screens/panel/fork_terminal.luau` (the
terminal tab) and the engine's `crates/engine/src/rpc/fork_session_card.rs`
(`ForkSessionCard`, and `RunProjectAction` for `npm:<script>` ids).

## Installing on a Mac

Plugins live in the app's data dir, not the bundle. From an agent with
Zeron's MCP tools, `create_plugin` with the id, name and permissions in the
manifest, then copy `init.luau` over the scaffold's; Zeron signs it as yours
and reloads it. Allow it once in Zeron. For another Mac, `export_plugin` there
or here and install the `.zeron-plugin` file.

The installed copy (`~/.zeron/plugins/<id>/`) is what runs: copy changes back
here after editing it.
