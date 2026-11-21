local zeron = require("./zeron")("clemann-session-card")

-- Session card: a card floating at a chat's top right, as the old desktop
-- app had it. Where the chat runs (its device), its project, its branch and
-- what the branch adds over its base (+N −M), then the project's npm
-- scripts: the ones you pinned on the card itself, and the rest in a list
-- that opens. A script runs in a terminal tab of the chat's panel, as the
-- panel home's run actions do.
--
-- The details and the runs go through the fork's bridge
-- (apps/zgx/src/plugins/fork_bridge.rs), which Zeron opens to this plugin
-- alone: `plugin.clemann-session-card.fork_card`.

local Glass = require("@ds/glass")
local Icon = require("@ds/icon")
local IconButton = require("@ds/icon_button").IconButton
local PanelHome = require("@ds/panel_home")

local CALL = "plugin.clemann-session-card.fork_card"
local W = 272

-- Pinned scripts by project (`{ [project]: { name } }`), and whether the
-- card is folded away; kept between runs.
local pins = zeron.store("pins", {})
local folded = zeron.store("folded", false)

local function pinned_in(project: string?): { string }
	if not project then
		return {}
	end
	return (pins:get() :: any)[project] or {}
end

local function is_pinned(project: string?, name: string): boolean
	return table.find(pinned_in(project), name) ~= nil
end

local function toggle_pin(project: string?, name: string)
	if not project then
		return
	end
	local all = table.clone(pins:peek() :: any)
	local list = table.clone(all[project] or {})
	local at = table.find(list, name)
	if at then
		table.remove(list, at)
	else
		table.insert(list, name)
	end
	all[project] = if #list > 0 then list else nil
	pins:set(all)
end

local function message(err: any): string
	local m = if type(err) == "table" and err.message then tostring(err.message) else tostring(err)
	if string.find(m, "fork_card", 1, true) or m == "no_bridge" then
		return "Update Zeron for branch changes and npm scripts"
	end
	return m
end

-- The bridge, as a promise; a build without it refuses the call up front,
-- which fails the promise instead of the card.
local function call(arg: { [string]: any }): any
	local ok, promise = pcall(ui.call, CALL, arg)
	if ok then
		return promise
	end
	return {
		after = function(_self: any, _done: any, failed: ((any) -> ())?)
			if failed then
				failed({ message = "no_bridge" })
			end
		end,
	}
end

-- A glyph that follows `name()`, for a row's icon.
local function glyph(name: () -> string): Node
	return ui.presence(name, function(now: string): Node
		return Icon { name = now :: any, size = PanelHome.ICON }
	end)
end

zeron.place {
	at = "#session-header",
	after = function(): Node
		local session = zeron.need("session")
		local id = session.id
		local function record(): any
			return zeron.context_of("session", id) or {}
		end
		local info = ui.use_state(nil :: any)
		local failed = ui.use_state(nil :: string?)
		local all_open = ui.use_state(false)

		local function load()
			call({ op = "info", session = id, device = record().device }):after(function(v: any)
				info:set(v)
				failed:set(nil)
			end, function(err: any)
				failed:set(message(err))
			end)
		end
		load()
		-- The branch's totals move as the agent works.
		ui.every(15000, load)

		local function scripts(): { any }
			local v = info:get()
			return if v and type(v.scripts) == "table" then v.scripts else {}
		end

		local function run(name: string)
			local r = record()
			if not r.project then
				zeron.toast("This chat has no project to run scripts in")
				return
			end
			call({ op = "run", session = id, project = r.project, device = r.device, script = name })
				:after(function() end, function(err: any)
					zeron.toast(message(err))
				end)
		end

		local function script_row(name: string, command: string?): Node
			return PanelHome.Row {
				icon = "action-play",
				label = name,
				tooltip = if command then `npm run {name}  ·  {command}` else `npm run {name}`,
				on_click = function()
					run(name)
				end,
				trail = IconButton {
					icon = "pin",
					size = "sm",
					tone = "quiet",
					lit = function(): boolean
						return is_pinned(record().project, name)
					end,
					label = function(): string
						return if is_pinned(record().project, name) then "Unpin from the card" else "Pin to the card"
					end,
					on_click = function()
						toggle_pin(record().project, name)
					end,
				},
			}
		end

		-- Rows for `names` (that package.json still has), rebuilt as the list moves.
		local function rows_of(names: () -> { string }, inset: any?): Node
			return ui.presence(function(): string
				local have: { [string]: string } = {}
				for _, s in scripts() do
					have[s.name] = s.command
				end
				local keep = {}
				for _, name in names() do
					if have[name] ~= nil then
						table.insert(keep, name .. "\0" .. have[name])
					end
				end
				return table.concat(keep, "\1")
			end, function(sig: string): Node
				local rows = {}
				if sig ~= "" then
					for _, entry in string.split(sig, "\1") do
						local parts = string.split(entry, "\0")
						table.insert(rows, script_row(parts[1], parts[2]))
					end
				end
				return ui.column { w_full = true, gap = 2, pl = inset, table.unpack(rows) }
			end)
		end

		local card = Glass.Panel {
			kind = "menu",
			class = "clemann-session-card",
			w = W,
			p = "$space.2",
			gap = 2,
			ui.row {
				w_full = true,
				h = 28,
				pl = "$space.2",
				items = "center",
				ui.text { "Session", grow = 1, text_size = "$font.size.xs", weight = "$font.weight.medium", color = "$text.tertiary" },
				IconButton { icon = "close", size = "sm", tone = "quiet", label = "Hide the card", on_click = function()
					folded:set(true)
				end },
			},
			PanelHome.Row {
				icon = glyph(function(): string
					local v = info:get()
					return if v and v.here == false then "remote-server" else "laptop"
				end),
				label = function(): string
					local v = info:get()
					if v and v.deviceName then
						return v.deviceName
					end
					return if v and v.here then "This Mac" else "…"
				end,
				tone = "muted",
			},
			PanelHome.Row {
				icon = "folder",
				label = function(): string
					local r = record()
					local p = if r.project then zeron.projects()[r.project] else nil
					return if p and p.name then p.name else "No project"
				end,
				tone = "muted",
			},
			ui.show(function(): boolean
				local v = info:get()
				return ((v and v.branch) or record().branch) ~= nil
			end, PanelHome.Row {
				icon = "git-branch",
				label = function(): string
					local v = info:get()
					return (v and v.branch) or record().branch or ""
				end,
				tooltip = function(): string
					local v = info:get()
					return if v and v.base then `Changes over {v.base}` else "Branch"
				end,
				tone = "muted",
				trail = ui.row {
					gap = "$space.1",
					shrink = 0,
					ui.text {
						function(): string
							local v = info:get()
							return if v and v.additions then `+{v.additions}` else ""
						end,
						font = "$font.mono",
						tabular = true,
						text_size = "$font.size.xs",
						color = "$zeron.diff.added",
					},
					ui.text {
						function(): string
							local v = info:get()
							return if v and v.deletions then `−{v.deletions}` else ""
						end,
						font = "$font.mono",
						tabular = true,
						text_size = "$font.size.xs",
						color = "$zeron.diff.removed",
					},
				},
			}),
			ui.show(function(): boolean
				return failed:get() ~= nil
			end, ui.text {
				function(): string
					return failed:get() or ""
				end,
				px = "$space.2",
				wrap = true,
				text_size = "$font.size.xs",
				color = "$text.tertiary",
			}),
			PanelHome.Heading { label = "Actions" },
			-- The pinned scripts, on the card itself (a pin whose script is
			-- gone from package.json shows no more).
			rows_of(function(): { string }
				return pinned_in(record().project)
			end),
			ui.show(function(): boolean
				return #scripts() > 0
			end, PanelHome.Row {
				icon = glyph(function(): string
					return if all_open:get() then "alt-arrow-down" else "alt-arrow-right"
				end),
				label = function(): string
					return `npm scripts ({#scripts()})`
				end,
				tone = "muted",
				on_click = function()
					all_open:set(not all_open:peek())
				end,
			}),
			ui.show(function(): boolean
				return all_open:get()
			end, rows_of(function(): { string }
				local names = {}
				for _, s in scripts() do
					table.insert(names, s.name)
				end
				return names
			end, "$space.3")),
			ui.show(function(): boolean
				return info:get() ~= nil and #scripts() == 0
			end, PanelHome.Row {
				icon = "info-circle",
				label = function(): string
					local v = info:get()
					return if v and v.scriptsError then v.scriptsError else "No npm scripts in this checkout"
				end,
				tone = "muted",
			}),
		}

		local reopen = Glass.Panel {
			kind = "island",
			p = 2,
			IconButton { icon = "info-circle", size = "md", tone = "normal", label = "Show the session card", on_click = function()
				folded:set(false)
			end },
		}

		-- A band across the page's top that the pointer passes through, the
		-- card at its right end (Zeron draws what's placed here over the
		-- page, so it floats over the transcript).
		return ui.row {
			w_full = true,
			justify = "end",
			pt = "$space.2",
			pr = "$space.4",
			pointer_events = "none",
			ui.column {
				pointer_events = "auto",
				ui.show(function(): boolean
					return folded:get() ~= true
				end, card),
				ui.show(function(): boolean
					return folded:get() == true
				end, reopen),
			},
		}
	end,
}
