# Changelog

## 0.4.1 (2026-11-12)

### Fixed

- `Esc` in a repository opened from the repository list goes back to the
  list, where you left it, instead of quitting gitenv; so does `s`. The
  footer says so.

## 0.4.0 (2026-11-07)

The terminal UI now does nearly everything the command line does, and
gitenv comes with a Neovim plugin.

### Fixing things from the TUI

- `f` lists this repository's unpushed commits made as someone else and
  re-authors the ones you tick. Unticked commits, such as a pair's, keep
  their author. Signed commits get the terminal back so gpg or ssh can ask
  for a passphrase.
- In the repository list, `space` marks a repository on the wrong profile,
  `a` marks all of them, and `F` fixes the marked ones after asking, as
  `gitenv scan --fix` does. `w` shows only the repositories not right yet.
- `u` undoes the last apply or fix, putting each repository's config back
  exactly as it was, unless it changed again since.
- `c` clones a repository onto the profile its remote rules name, or one
  you pick, as `gitenv clone` does.

### Seeing what is going on

- Profiles carry a `✗` or `!` when doctor's local checks find a problem,
  such as a missing key file. `D` shows every finding for a profile and
  checks its ssh and gh logins in the background.
- A *Setup* section says whether the hooks check commits and whether git
  applies your rules through the includes. `H` and `I` install them after
  asking.
- `w` explains why a repository belongs to its profile: every matching
  rule, the winner, and why each other one lost.
- `R` shows your commits by profile for the last 7 or 30 days, this month
  or last month, counting those made as another identity.

### Editing profiles

- `N` starts a profile from what git uses in this repository, with a rule
  for its owner.
- The form checks fields as you type, such as an ssh key path with no file
  or the `.pub` half of a key, and `Ctrl-O` picks ssh keys, gpg keys,
  formats and gh accounts from lists.
- The rule fields preview which nearby repositories the rules would claim,
  and which stay with another profile's rule.

### Getting around

- `/` filters the profiles and the repository list as you type.
- The TUI reloads by itself when the profiles file or the repository
  changes on disk, for example after a commit in another terminal.
- The mouse works: the wheel scrolls, a click selects. `GITENV_MOUSE=0`
  turns it off.
- `GITENV_THEME=light` suits a pale terminal, and `GITENV_THEME=none` or
  `NO_COLOR` drops colour.

### Neovim

- The repository is a Neovim plugin (0.10 or newer):
  `{ "mwala-zm/gitenv", opts = {} }` with lazy.nvim. `:Gitenv` opens the
  TUI in a floating window, `:GitenvApply`, `:GitenvStatus` and
  `:GitenvFixAuthors` work without it, a statusline component shows the
  profile, and opening a repository on the wrong profile warns once. See
  `:help gitenv`.

### Command line

- `gitenv profiles` lists the profile names, one per line, for completion.

### Changed

- The TUI rewrites exported includes when it starts and when the profiles
  file is edited by hand, not only when it saves a profile, so they always
  match your profiles.
- The help popup no longer explains what applying writes; the README does.

## Earlier releases

0.3.0 and before are described by the commits up to the `v0.3.0` tag.
