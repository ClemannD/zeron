--!strict
-- Fork: an npm script the session card plugin ran (src/plugins/fork_bridge.rs),
-- as a terminal tab of the session's panel, the way the panel home's run
-- actions open (screens/panel/init.luau `open_action`): the tab carries the
-- run, and screens/panel/terminal starts it through `home.run_action`, which
-- the engine answers for an `npm:<script>` id (rpc/fork_session_card.rs).
-- Each click is a new tab, as each run is a new terminal.

local state = require("@app/screens/panel/state")

local M = {}

function M.open(asked: { [string]: any })
	local session, project = asked.session, asked.project
	if type(session) ~= "string" or type(project) ~= "string" or type(asked.action) ~= "string" then
		return
	end
	local k = state.key(session, nil)
	if not k then
		return
	end
	local device = if type(asked.device) == "string" then asked.device else nil
	state.open(k, "terminal", {
		title = if type(asked.title) == "string" then asked.title else asked.action,
		run = { space = project, chat = session, device = device, action = asked.action },
	})
	-- The panel open for the session, as a plugin's tab opens it.
	local ok, view = pcall(function()
		return (require :: any)("@app/screens/shell/view")
	end)
	if ok and type(view) == "table" and view.set_files then
		view.set_files(true)
	end
end

return M
