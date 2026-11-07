if vim.g.loaded_gitenv then
  return
end
vim.g.loaded_gitenv = true

vim.api.nvim_set_hl(0, "GitenvOk", { link = "DiagnosticOk", default = true })
vim.api.nvim_set_hl(0, "GitenvWrong", { link = "DiagnosticWarn", default = true })
vim.api.nvim_set_hl(0, "GitenvError", { link = "DiagnosticError", default = true })

local function gitenv()
  return require("gitenv")
end

local command = vim.api.nvim_create_user_command

command("Gitenv", function(o)
  gitenv().open(o.args ~= "" and o.args or nil)
end, { nargs = "?", complete = "dir", desc = "Open the gitenv TUI in a floating window" })

command("GitenvApply", function(o)
  gitenv().apply(o.args)
end, {
  nargs = "?",
  complete = function(lead)
    return gitenv().profiles(lead)
  end,
  desc = "Apply a profile, or the one the rules name, to this repository",
})

command("GitenvStatus", function()
  gitenv().show_status()
end, { desc = "Show which identity git uses here and whether it is right" })

command("GitenvFixAuthors", function()
  gitenv().fix_authors()
end, { desc = "Re-author unpushed commits made as someone else" })

local group = vim.api.nvim_create_augroup("gitenv", { clear = true })
vim.api.nvim_create_autocmd({ "BufEnter", "DirChanged" }, {
  group = group,
  callback = function(ev)
    if vim.bo[ev.buf].buftype == "" then
      gitenv().refresh(gitenv().root(ev.buf))
    end
  end,
})
vim.api.nvim_create_autocmd("FocusGained", {
  group = group,
  callback = function()
    gitenv().refresh(nil, { force = true })
  end,
})
