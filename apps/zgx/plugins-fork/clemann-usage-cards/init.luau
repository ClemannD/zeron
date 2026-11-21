local zeron = require("./zeron")("clemann-usage-cards")

-- Usage cards: your agents' plan usage at the foot of the sidebar, a card per
-- provider showing its active account's meters (the old desktop's sidebar
-- usage cards). What Zeron last read (`zeron.usage`), kept fresh by its
-- `usage.changed` events: reading never makes Zeron ask a provider again.

local Mark = require("@ds/mark")
local Progress = require("@ds/progress")

-- A provider's name and mark, by its harness id.
local PROVIDERS: { [string]: { name: string, mark: string? } } = {
	["claude-code"] = { name = "Claude", mark = "claude" },
	codex = { name = "Codex", mark = "openai" },
	cursor = { name = "Cursor", mark = "cursor" },
	grok = { name = "Grok", mark = "grok" },
	antigravity = { name = "Antigravity", mark = "antigravity" },
	devin = { name = "Devin", mark = "devin" },
	muse = { name = "Muse", mark = "muse" },
	gemini = { name = "Gemini", mark = "provider-gemini" },
}
-- Settings' order; others after them, by id.
local ORDER = { "claude-code", "codex", "cursor", "grok", "antigravity", "gemini", "devin", "muse" }

local function rank(provider: string): number
	for i, id in ORDER do
		if id == provider then
			return i
		end
	end
	return #ORDER + 1
end

-- Each provider's active account that has meters to show, in order: of
-- the providers switched on here (`on`, by id), or all when that's unknown.
local function cards(list: { any }, on: { [string]: boolean }?): { any }
	local out = {}
	for _, a in list do
		if a.active and type(a.windows) == "table" and #a.windows > 0 and (on == nil or on[a.provider]) then
			table.insert(out, a)
		end
	end
	table.sort(out, function(a, b)
		local ra, rb = rank(a.provider), rank(b.provider)
		if ra ~= rb then
			return ra < rb
		end
		return tostring(a.provider) < tostring(b.provider)
	end)
	return out
end

-- What a card shows, as one key: a card is built again when it changes.
local function signature(a: any): string
	local parts = { tostring(a.provider), tostring(a.account), tostring(a.plan), tostring(a.error ~= nil), tostring(a.fetched_at) }
	for _, w in a.windows do
		table.insert(parts, `{w.label}:{w.used}:{w.resets_at}`)
	end
	return table.concat(parts, "|")
end

local function ago(ms: number?): string
	if type(ms) ~= "number" then
		return "Cached"
	end
	local minutes = math.max(0, math.floor((os.time() * 1000 - ms) / 60000))
	if minutes < 60 then
		return string.format("Cached · updated %dm ago", minutes)
	elseif minutes < 1440 then
		return string.format("Cached · updated %dh ago", minutes // 60)
	end
	return string.format("Cached · updated %dd ago", minutes // 1440)
end

local function until_text(ms: number?): string?
	if type(ms) ~= "number" then
		return nil
	end
	local secs = math.max(0, math.floor(ms / 1000 - os.time()))
	if secs < 3600 then
		return string.format("%dm", math.max(1, secs // 60))
	elseif secs < 86400 then
		return string.format("%dh %dm", secs // 3600, (secs % 3600) // 60)
	end
	return string.format("%dd %dh", secs // 86400, (secs % 86400) // 3600)
end

-- When each window starts over, for the card's tooltip.
local function resets(windows: { any }): string
	local lines = {}
	for _, w in windows do
		local left = until_text(w.resets_at)
		if left then
			table.insert(lines, string.format("%s resets in %s", w.label or "Usage", left))
		end
	end
	return if #lines > 0 then table.concat(lines, "\n") else "Usage"
end

-- One window's meter, as Settings → Providers draws it: its name, a thin
-- bar filling the room between, the percent in the bar's tone past 75%.
local function meter(w: any): Node
	local used = math.clamp(tonumber(w.used) or 0, 0, 100)
	local tone = if used >= 90 then "danger" elseif used >= 75 then "warning" else "accent"
	return ui.row {
		w_full = true,
		min_w = 0,
		gap = "$space.2",
		items = "center",
		ui.text { w.label or "", w = 52, shrink = 0, truncate = true, text_size = "$font.size.xs", line_height = "$font.line.xs", color = "$text.tertiary" },
		ui.div {
			grow = 1,
			min_w = 0,
			Progress.ProgressBar {
				value = function(): number?
					return used / 100
				end,
				tone = tone,
				size = "thin",
				w_full = true,
				label = `{w.label or "Usage"} usage`,
			},
		},
		ui.text {
			string.format("%d%%", math.floor(used + 0.5)),
			w = 34,
			shrink = 0,
			align = "right",
			tabular = true,
			text_size = "$font.size.xs",
			line_height = "$font.line.xs",
			color = if tone == "accent" then "$text.tertiary" else `$zeron.tone.{tone}.ink`,
		},
	}
end

local function card(a: any): Node
	local p = PROVIDERS[a.provider] or { name = tostring(a.provider) }
	local rows = {}
	for _, w in a.windows do
		table.insert(rows, meter(w))
	end
	return ui.column {
		w_full = true,
		min_w = 0,
		px = "$space.2",
		py = 6,
		gap = 2,
		rounded = "$radius.md",
		bg = "$hover",
		tooltip = resets(a.windows),
		ui.row {
			w_full = true,
			gap = 6,
			items = "center",
			if p.mark then ui.div { shrink = 0, color = "$text.primary", Mark { name = p.mark, size = 13, bare = true } } else nil,
			ui.text { p.name, grow = 1, min_w = 0, truncate = true, text_size = "$font.size.xs", weight = "$font.weight.medium", color = "$text.primary" },
			if a.plan then ui.text { a.plan, shrink = 0, text_size = "$font.size.xs", color = "$text.tertiary" } else nil,
		},
		ui.column { w_full = true, gap = 2, table.unpack(rows) },
		if a.error then ui.text { ago(a.fetched_at), text_size = "$font.size.xs", color = "$text.tertiary" } else nil,
	}
end

-- The cards, for every window of this plugin: read once, then as Zeron
-- says usage changed, and each minute (reset times move on).
local shown = ui.state("clemann_usage_cards", {} :: { any })
-- The providers switched on and installed here, through the fork's bridge
-- (src/plugins/fork_bridge.rs); nil from a build without it.
local function providers(done: ({ [string]: boolean }?) -> ())
	local ok, promise = pcall(ui.call, "plugin.clemann-usage-cards.fork_card", { op = "providers" })
	if not ok then
		done(nil)
		return
	end
	promise:after(function(list: any)
		local on = {}
		for _, p in list or {} do
			on[p.id] = true
		end
		done(on)
	end, function()
		done(nil)
	end)
end

local function load()
	providers(function(on)
		zeron.usage():after(function(got: any)
			shown:set(cards(got or {}, on))
		end, function() end)
	end)
end
load()
zeron.on("usage.changed", function()
	load()
end)
zeron.every(60, load)

zeron.place {
	at = "#sidebar",
	after = function(): Node
		return ui.show(function(): boolean
			return #shown:get() > 0
		end, ui.column {
			w_full = true,
			shrink = 0,
			px = "$space.2",
			pb = "$space.2",
			gap = "$space.1",
			ui.each(shown, function(a: any): Node
				return card(a)
			end, { key = signature }),
		})
	end,
}
