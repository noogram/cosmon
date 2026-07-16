-- cs-drop.hammerspoon.lua — global macOS hotkey for the universal Inbox drop.
--
-- First-spike implementation of task-20260424-86e9 (C-DROP-GESTURE).
-- Binds ⌃⌥D to a minimal textual prompt that pipes into `cs drop`.
-- Landed as Hammerspoon rather than native SwiftUI because Hammerspoon
-- ships today with zero additional Xcode scaffolding — the native
-- migration (NSEvent.addGlobalMonitorForEvents + a mac-pilot SwiftUI
-- sheet) is a follow-up. Chord choice (`⌃⌥D`) is the task briefing's
-- documented fallback; no clash with Spotlight (⌘Space), Raycast, or
-- macOS Dictation (fn-fn).
--
-- Install:
--   ln -s <repo>/scripts/cs-drop.hammerspoon.lua ~/.hammerspoon/cs-drop.lua
--   and add `require("cs-drop")` at the top of ~/.hammerspoon/init.lua.
--   Reload Hammerspoon (menu bar → Reload Config) after the symlink.
--
-- Tune:
--   Override the binary path by setting `hs.cs_drop_binary` before the
--   require, e.g. `hs.cs_drop_binary = "/opt/homebrew/bin/cs"`.
--   Override the chord by setting `hs.cs_drop_chord` to a table like
--   `{ {"ctrl","alt"}, "d" }` before the require.

local M = {}

local CS_BIN = hs.cs_drop_binary or (os.getenv("HOME") .. "/.cargo/bin/cs")
local CHORD  = hs.cs_drop_chord  or { { "ctrl", "alt" }, "d" }

-- Shell out to `cs drop` with the given text. Non-blocking — feedback
-- flashes an alert (success / failure) so the operator sees the
-- nucleation result without switching focus.
local function fire(text)
  if not text or text:match("^%s*$") then return end
  local task = hs.task.new(
    CS_BIN,
    function(exitCode, stdOut, stdErr)
      if exitCode == 0 then
        -- Look for the spark-YYYYMMDD-xxxx id in stdout.
        local id = stdOut and stdOut:match("(spark%-[%w%-]+)") or nil
        hs.alert.show("cosmon ✦ " .. (id or "drop dispatched"), 1.5)
      else
        local msg = (stdErr and stdErr ~= "") and stdErr or stdOut or "unknown error"
        hs.alert.show("cs drop failed: " .. msg, 3)
      end
    end,
    { "drop", text }
  )
  task:start()
end

-- Prompt the operator for drop text. Uses Hammerspoon's built-in
-- hs.dialog.textPrompt for the first spike; a native SwiftUI sheet
-- comes later (task-20260424-86e9 §4, mac-pilot menubar migration).
local function prompt()
  local ok, text = hs.dialog.textPrompt(
    "cosmon drop",
    "Que s'est-il passé dans ta tête ?",
    "",
    "Drop",
    "Cancel"
  )
  if ok == "Drop" and text and text ~= "" then
    fire(text)
  end
end

-- Bind the chord. hs.hotkey.bind respects an already-bound chord by
-- overwriting; reloading init.lua is idempotent.
M.hotkey = hs.hotkey.bind(CHORD[1], CHORD[2], prompt)

-- Expose `fire` so other hammerspoon modules can shell into `cs drop`
-- without re-implementing the plumbing (e.g. a scripted Shortcut).
M.fire = fire

return M
