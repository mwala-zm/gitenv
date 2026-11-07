local M = {}

function M.check()
  local health = vim.health
  health.start("gitenv")
  if vim.fn.has("nvim-0.10") == 1 then
    health.ok("Neovim " .. tostring(vim.version()))
  else
    health.error("Neovim 0.10 or newer is needed")
  end

  local cmd = require("gitenv").config.cmd
  if vim.fn.executable(cmd) ~= 1 then
    health.error(("%s is not on your PATH"):format(cmd), {
      "brew install mwala-zm/tap/gitenv",
      "cargo install gitenv",
      "or set `cmd` in require('gitenv').setup() to the binary's full path",
    })
    return
  end
  local version = vim.system({ cmd, "--version" }, { text = true }):wait(2000)
  health.ok(("%s (%s)"):format(vim.trim(version.stdout or "gitenv"), vim.fn.exepath(cmd)))

  local profiles = vim.system({ cmd, "profiles" }, { text = true }):wait(2000)
  if profiles.code == 0 then
    local n = #vim.split(profiles.stdout or "", "\n", { trimempty = true })
    health.ok(("%d profile(s), so :GitenvApply can complete their names"):format(n))
  elseif (profiles.stderr or ""):match("unrecognized subcommand") then
    health.warn("this gitenv has no `profiles` command, so :GitenvApply cannot complete names", {
      "update gitenv to 0.4.0 or newer",
    })
  else
    health.error("gitenv could not read the profiles file: " .. vim.trim(profiles.stderr or ""))
  end
end

return M
