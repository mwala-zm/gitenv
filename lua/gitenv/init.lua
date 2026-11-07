local M = {}

local defaults = {
  cmd = "gitenv",
  warn = "notify",
  max_age = 5,
  float = { width = 0.85, height = 0.85, border = "rounded" },
}

M.config = vim.deepcopy(defaults)

local cache = {}
local warned = {}

---@class gitenv.Status
---@field token string what `gitenv status --short` printed, or `!` when it could not look
---@field code integer 0 on the right profile, 1 on the wrong one or none, 2 when it could not look
---@field actual string|nil the profile the identity is on
---@field expected string|nil the profile a rule says the repository belongs to
---@field error string|nil why gitenv could not look
---@field at number when it was read, from `vim.uv.now()`

---Merges `opts` over the defaults. Calling it is optional.
---@param opts table|nil
function M.setup(opts)
  M.config = vim.tbl_deep_extend("force", vim.deepcopy(defaults), opts or {})
end

---The repository holding buffer `buf`'s file, or the working directory's
---for unnamed and special buffers such as terminals and help.
---@param buf integer|nil
---@return string|nil
function M.root(buf)
  buf = buf or vim.api.nvim_get_current_buf()
  if vim.bo[buf].buftype == "" and vim.api.nvim_buf_get_name(buf) ~= "" then
    local root = vim.fs.root(buf, ".git")
    if root then
      return root
    end
  end
  return vim.fs.root(vim.fn.getcwd(), ".git")
end

---Splits a `--short` token: `acme`, `personal -> acme`, `? -> acme` or `?`.
---@param token string
---@param code integer
---@return gitenv.Status
local function parse(token, code)
  local actual, expected = token:match("^(.-) %-> (.+)$")
  if not actual then
    actual = token
  end
  return {
    token = token,
    code = code,
    actual = actual ~= "?" and actual or nil,
    expected = expected,
    at = vim.uv.now(),
  }
end

local function notify(msg, level)
  vim.notify(msg, level or vim.log.levels.INFO, { title = "gitenv" })
end

local function available()
  return vim.fn.executable(M.config.cmd) == 1
end

---Tells the user, once per repository and state, that it is on the wrong
---profile or could not be checked, as `config.warn` says: a notification,
---a prompt offering to apply the rule's profile or open the TUI, or nothing.
---@param root string
---@param s gitenv.Status
local function warn(root, s)
  if s.code == 0 then
    warned[root] = nil
    return
  end
  if not M.config.warn or warned[root] == s.token then
    return
  end
  warned[root] = s.token
  local name = vim.fn.fnamemodify(root, ":~")
  if s.code == 2 then
    notify(("could not check %s: %s"):format(name, s.error), vim.log.levels.ERROR)
    return
  end
  local msg
  if s.expected and s.actual then
    msg = ("%s is on profile %s, but a rule says %s"):format(name, s.actual, s.expected)
  elseif s.expected then
    msg = ("%s matches no profile, but a rule says %s"):format(name, s.expected)
  else
    msg = ("%s's identity matches no profile"):format(name)
  end
  if M.config.warn ~= "prompt" then
    local hint = s.expected and " (:GitenvApply switches it)" or " (:Gitenv to pick one)"
    notify(msg .. hint, vim.log.levels.WARN)
    return
  end
  local apply = s.expected and ("Apply " .. s.expected) or nil
  local choices = { "Open gitenv", "Not now" }
  if apply then
    table.insert(choices, 1, apply)
  end
  vim.ui.select(choices, { prompt = msg }, function(choice)
    if choice == apply and apply then
      M.apply(nil, root)
    elseif choice == "Open gitenv" then
      M.open(root)
    end
  end)
end

---Reads the repository's status in the background and caches it, unless a
---read younger than `max_age` seconds is cached (with `opts.force`, always).
---A read asked for while one runs happens once that one is done. Fires
---`User GitenvStatus` and redraws the statuslines when it lands.
---@param root string|nil defaults to the current buffer's repository
---@param opts {force: boolean}|nil
function M.refresh(root, opts)
  root = root or M.root()
  if not root or not available() then
    return
  end
  local entry = cache[root]
  if entry and entry.pending then
    entry.again = true
    return
  end
  local force = opts and opts.force
  if entry and entry.at and not force and vim.uv.now() - entry.at < M.config.max_age * 1000 then
    return
  end
  cache[root] = vim.tbl_extend("force", entry or {}, { pending = true })
  local ok = pcall(vim.system, { M.config.cmd, "status", "--short", root }, { text = true }, function(out)
    vim.schedule(function()
      local s
      if out.code == 2 or out.signal ~= 0 then
        local err = vim.trim((out.stderr or "") ~= "" and out.stderr or (out.stdout or ""))
        s = { token = "!", code = 2, error = err:gsub("^gitenv: ", ""), at = vim.uv.now() }
      else
        s = parse(vim.trim(out.stdout or ""), out.code)
      end
      local again = cache[root] and cache[root].again
      cache[root] = s
      vim.api.nvim_exec_autocmds("User", { pattern = "GitenvStatus", data = { root = root, status = s } })
      vim.cmd("redrawstatus!")
      warn(root, s)
      if again then
        M.refresh(root, { force = true })
      end
    end)
  end)
  if not ok then
    cache[root] = entry
  end
end

---The cached status of the current buffer's repository, starting a read
---when there is none yet. Nil outside a repository or before the first read.
---@return gitenv.Status|nil
function M.status()
  local root = M.root()
  if not root then
    return nil
  end
  local s = cache[root]
  if not s or (not s.at and not s.pending) then
    M.refresh(root)
  end
  return s and s.token and s or nil
end

---The status token for a statusline, or an empty string. With
---`{ highlight = true }` it carries its own highlight group, for a plain
---`statusline` option.
---@param opts {highlight: boolean}|nil
---@return string
function M.statusline(opts)
  local s = M.status()
  if not s then
    return ""
  end
  if opts and opts.highlight then
    return ("%%#%s#%s%%*"):format(M.color(), s.token)
  end
  return s.token
end

---The highlight group for the current status: `GitenvOk`, `GitenvWrong`
---or `GitenvError`. Fits lualine's `color` option.
---@return string
function M.color()
  local s = M.status()
  if not s or s.code == 0 then
    return "GitenvOk"
  end
  return s.code == 1 and "GitenvWrong" or "GitenvError"
end

---Profile names for completion, filtered by what was typed so far.
---@param lead string|nil
---@return string[]
function M.profiles(lead)
  if not available() then
    return {}
  end
  local ok, out = pcall(function()
    return vim.system({ M.config.cmd, "profiles" }, { text = true }):wait(2000)
  end)
  if not ok or out.code ~= 0 then
    return {}
  end
  return vim.tbl_filter(function(name)
    return vim.startswith(name, lead or "")
  end, vim.split(out.stdout or "", "\n", { trimempty = true }))
end

---Runs a gitenv subcommand without a terminal, shows what it printed, and
---re-reads the repository's status.
---@param args string[]
---@param root string
local function run(args, root)
  local cmd = vim.list_extend({ M.config.cmd }, args)
  vim.system(cmd, { text = true }, function(out)
    vim.schedule(function()
      local text = vim.trim((out.stdout or "") .. "\n" .. (out.stderr or ""))
      notify(text ~= "" and text or "done", out.code == 0 and vim.log.levels.INFO or vim.log.levels.WARN)
      M.refresh(root, { force = true })
    end)
  end)
end

local function need_root(root)
  root = root or M.root()
  if not root then
    notify("not inside a git repository", vim.log.levels.ERROR)
  end
  return root
end

local function need_cmd()
  if available() then
    return true
  end
  notify(("%s is not on your PATH; see :checkhealth gitenv"):format(M.config.cmd), vim.log.levels.ERROR)
  return false
end

---Writes profile `name` into the repository, or without a name the profile
---whose rules claim it, as `gitenv apply` does.
---@param name string|nil
---@param root string|nil
function M.apply(name, root)
  root = need_root(root)
  if not root or not need_cmd() then
    return
  end
  local args = { "apply" }
  if name and name ~= "" then
    table.insert(args, name)
  end
  vim.list_extend(args, { "--path", root })
  run(args, root)
end

---Shows `gitenv status` for the repository.
---@param root string|nil
function M.show_status(root)
  root = need_root(root)
  if root and need_cmd() then
    run({ "status", root }, root)
  end
end

---Runs gitenv in a floating terminal. With `close` the window goes when the
---program exits; otherwise it stays to show the output until a key is
---pressed. Either way the status is read again and changed buffers are
---reloaded, since applying a profile or rewriting commits changes the repo.
---@param args string[]
---@param root string|nil
---@param close boolean
local function float(args, root, close)
  local cfg = M.config.float
  local lines = vim.o.lines - vim.o.cmdheight
  local width = math.max(40, math.floor(vim.o.columns * cfg.width))
  local height = math.max(10, math.floor(lines * cfg.height))
  local buf = vim.api.nvim_create_buf(false, true)
  local win = vim.api.nvim_open_win(buf, true, {
    relative = "editor",
    width = math.min(width, vim.o.columns),
    height = math.min(height, lines),
    row = math.max(0, math.floor((lines - height) / 2) - 1),
    col = math.max(0, math.floor((vim.o.columns - width) / 2)),
    style = "minimal",
    border = cfg.border,
    title = " gitenv ",
    title_pos = "center",
  })
  local cmd = vim.list_extend({ M.config.cmd }, args)
  local opts = {
    cwd = root,
    on_exit = function()
      vim.schedule(function()
        if close and vim.api.nvim_win_is_valid(win) then
          vim.api.nvim_win_close(win, true)
        end
        if close and vim.api.nvim_buf_is_valid(buf) then
          vim.api.nvim_buf_delete(buf, { force = true })
        end
        if root then
          M.refresh(root, { force = true })
        end
        vim.cmd("silent! checktime")
      end)
    end,
  }
  if vim.fn.has("nvim-0.11") == 1 then
    opts.term = true
    vim.fn.jobstart(cmd, opts)
  else
    vim.fn.termopen(cmd, opts)
  end
  vim.api.nvim_create_autocmd("VimResized", {
    buffer = buf,
    callback = function()
      if vim.api.nvim_win_is_valid(win) then
        local l = vim.o.lines - vim.o.cmdheight
        vim.api.nvim_win_set_config(win, {
          relative = "editor",
          width = math.min(math.max(40, math.floor(vim.o.columns * cfg.width)), vim.o.columns),
          height = math.min(math.max(10, math.floor(l * cfg.height)), l),
          row = math.max(0, math.floor(l * (1 - cfg.height) / 2) - 1),
          col = math.max(0, math.floor(vim.o.columns * (1 - cfg.width) / 2)),
        })
      end
    end,
  })
  vim.cmd.startinsert()
  return win, buf
end

---Opens the gitenv TUI in a floating window on `path`, or on the current
---buffer's repository, or the working directory; outside a repository it
---opens on the list of repositories there.
---@param path string|nil
function M.open(path)
  if not need_cmd() then
    return
  end
  local target = path and vim.fn.fnamemodify(vim.fn.expand(path), ":p") or M.root() or vim.fn.getcwd()
  return float({ target }, vim.fs.root(target, ".git"), true)
end

---Re-authors the repository's unpushed commits made as someone else with
---`gitenv fix-authors`, in a floating terminal so it can ask first.
---@param root string|nil
function M.fix_authors(root)
  root = need_root(root)
  if root and need_cmd() then
    return float({ "fix-authors", "--path", root }, root, false)
  end
end

return M
