-- Run from the repository root after `cargo build`:
--   nvim --headless -u NONE -l tests/nvim/run.lua
-- Uses target/debug/gitenv, a throwaway profiles file and repository, and
-- no global or system git config.

local here = vim.fn.getcwd()
vim.opt.rtp:prepend(here)
vim.o.swapfile = false
vim.o.showmode = false

local function say(text)
  io.stdout:write(text .. "\n")
end

local tmp = vim.fn.resolve(vim.fn.tempname())
local repo = tmp .. "/work/acme/api"
vim.fn.mkdir(repo, "p")
vim.fn.writefile({}, tmp .. "/gitconfig")
vim.env.GIT_CONFIG_GLOBAL = tmp .. "/gitconfig"
vim.env.GIT_CONFIG_NOSYSTEM = "1"
vim.env.GITENV_CONFIG = tmp .. "/config.toml"
vim.fn.writefile({
  "[[profiles]]",
  'name = "personal"',
  'user_name = "Ada"',
  'email = "ada@example.com"',
  "",
  "[[profiles]]",
  'name = "acme"',
  'user_name = "Ada"',
  'email = "ada@acme.example"',
  ('paths = ["%s/work/acme/**"]'):format(tmp),
}, vim.env.GITENV_CONFIG)

local function git(...)
  local out = vim.system({ "git", "-C", repo, ... }, { text = true }):wait()
  assert(out.code == 0, out.stderr)
  return vim.trim(out.stdout)
end
git("init", "-q")
git("config", "user.name", "Ada")
git("config", "user.email", "ada@example.com")

local notes = {}
vim.notify = function(msg, level)
  table.insert(notes, { msg = msg, level = level })
end

vim.cmd.runtime("plugin/gitenv.lua")
local gitenv = require("gitenv")
gitenv.setup({ cmd = here .. "/target/debug/gitenv" })

local failures = 0
local function test(name, fn)
  local ok, err = pcall(fn)
  say((ok and "ok   " or "FAIL ") .. name .. (ok and "" or ("\n     " .. tostring(err))))
  if not ok then
    failures = failures + 1
  end
end

local function eq(actual, expected, what)
  assert(vim.deep_equal(actual, expected), ("%s: expected %s, got %s"):format(what, vim.inspect(expected), vim.inspect(actual)))
end

local function wait_for(what, fn)
  assert(vim.wait(5000, fn, 20), "timed out waiting for " .. what)
end

test("the statusline shows the wrong profile and the rule's", function()
  vim.cmd.edit(repo .. "/README.md")
  wait_for("a status", function()
    return gitenv.status() ~= nil
  end)
  eq(gitenv.statusline(), "personal -> acme", "token")
  eq(gitenv.color(), "GitenvWrong", "colour")
  eq(gitenv.statusline({ highlight = true }), "%#GitenvWrong#personal -> acme%*", "highlighted")
  eq(gitenv.status().expected, "acme", "expected")
end)

test("opening a repo on the wrong profile warns once", function()
  wait_for("a warning", function()
    return #notes > 0
  end)
  assert(notes[1].msg:find("is on profile personal, but a rule says acme", 1, true), notes[1].msg)
  eq(notes[1].level, vim.log.levels.WARN, "level")
  gitenv.refresh(nil, { force = true })
  vim.wait(500)
  eq(#notes, 1, "warnings")
end)

test("profile names complete", function()
  eq(vim.fn.getcompletion("GitenvApply ", "cmdline"), { "personal", "acme" }, "all")
  eq(vim.fn.getcompletion("GitenvApply a", "cmdline"), { "acme" }, "filtered")
end)

test(":GitenvApply applies the rule's profile and the status follows", function()
  notes = {}
  vim.cmd("GitenvApply")
  wait_for("the right profile", function()
    local s = gitenv.status()
    return s and s.token == "acme"
  end)
  eq(git("config", "user.email"), "ada@acme.example", "email")
  eq(gitenv.color(), "GitenvOk", "colour")
  assert(notes[1] and notes[1].msg:find("acme", 1, true), vim.inspect(notes))
end)

test("a broken profiles file is reported as an error", function()
  local good = vim.fn.readfile(vim.env.GITENV_CONFIG)
  vim.fn.writefile({ "not toml [" }, vim.env.GITENV_CONFIG)
  notes = {}
  gitenv.refresh(nil, { force = true })
  wait_for("an error", function()
    local s = gitenv.status()
    return s and s.code == 2
  end)
  eq(gitenv.statusline(), "!", "token")
  eq(gitenv.color(), "GitenvError", "colour")
  assert(notes[1] and notes[1].level == vim.log.levels.ERROR, vim.inspect(notes))
  vim.fn.writefile(good, vim.env.GITENV_CONFIG)
  gitenv.refresh(nil, { force = true })
  wait_for("recovery", function()
    return gitenv.status().code == 0
  end)
end)

test(":Gitenv opens the TUI in a float and q closes it", function()
  local before = vim.api.nvim_get_current_win()
  vim.cmd("Gitenv")
  local win = vim.api.nvim_get_current_win()
  assert(win ~= before, "no new window")
  assert(vim.api.nvim_win_get_config(win).relative ~= "", "not floating")
  local buf = vim.api.nvim_win_get_buf(win)
  eq(vim.bo[buf].buftype, "terminal", "buftype")
  wait_for("the TUI to draw", function()
    return table.concat(vim.api.nvim_buf_get_lines(buf, 0, -1, false), "\n"):find("Profiles", 1, true) ~= nil
  end)
  vim.api.nvim_chan_send(vim.bo[buf].channel, "q")
  wait_for("the float to close", function()
    return not vim.api.nvim_win_is_valid(win)
  end)
  assert(not vim.api.nvim_buf_is_valid(buf), "buffer left behind")
end)

test(":GitenvFixAuthors keeps its output on screen", function()
  git("-c", "user.email=bob@example.com", "commit", "-q", "--allow-empty", "-m", "wrong")
  vim.cmd("GitenvFixAuthors")
  local win = vim.api.nvim_get_current_win()
  local buf = vim.api.nvim_win_get_buf(win)
  wait_for("the question", function()
    return table.concat(vim.api.nvim_buf_get_lines(buf, 0, -1, false), "\n"):find("Rewrite them?", 1, true) ~= nil
  end)
  vim.api.nvim_chan_send(vim.bo[buf].channel, "y\r")
  wait_for("the rewrite", function()
    return git("log", "-1", "--format=%ae") == "ada@acme.example"
  end)
  wait_for("the program to exit", function()
    return vim.fn.jobwait({ vim.bo[buf].channel }, 0)[1] ~= -1
  end)
  assert(vim.api.nvim_win_is_valid(win), "the output window closed")
  vim.api.nvim_win_close(win, true)
end)

test("warn = prompt offers to apply the rule's profile", function()
  gitenv.setup({ cmd = here .. "/target/debug/gitenv", warn = "prompt" })
  local offered
  vim.ui.select = function(choices, opts, on_choice)
    offered = { choices = choices, prompt = opts.prompt }
    on_choice(choices[1])
  end
  git("config", "user.email", "ada@example.com")
  gitenv.refresh(nil, { force = true })
  wait_for("the prompt", function()
    return offered ~= nil
  end)
  eq(offered.choices, { "Apply acme", "Open gitenv", "Not now" }, "choices")
  wait_for("the profile to be applied", function()
    return git("config", "user.email") == "ada@acme.example"
  end)
end)

test(":checkhealth gitenv passes", function()
  vim.cmd("checkhealth gitenv")
  local text = table.concat(vim.api.nvim_buf_get_lines(0, 0, -1, false), "\n")
  assert(text:find("profile(s), so :GitenvApply", 1, true), text)
  assert(not text:find("ERROR", 1, true), text)
end)

vim.fn.delete(tmp, "rf")
if failures > 0 then
  say(failures .. " failed")
  vim.cmd("cquit 1")
end
say("all passed")
vim.cmd("qall!")
