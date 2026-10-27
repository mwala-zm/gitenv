# gitenv

Commit with the right git identity in every repository.

![gitenv showing a repository on the personal profile that a rule says belongs to the acme profile](docs/tui.png)

If you work for more than one client, team or open source project, you have
probably pushed a commit with the wrong name, email, signing key or SSH key
at least once. gitenv keeps one profile per identity and knows which
repositories belong to which profile, so it can:

- show at a glance which identity git will use here, and whether it is the
  right one, then switch with one key press;
- make git pick the right identity by itself, even in a fresh clone,
  through git's own conditional includes;
- refuse a commit or push made as the wrong person, from a hook;
- audit every repository under a folder and fix them all at once;
- check that each profile's keys exist and log in as the right account;
- re-author the commits you made as the wrong person before you push them.

## Install

gitenv shells out to `git`, so git must be installed.

**Homebrew** (macOS and Linux):

```sh
brew install mwala-zm/tap/gitenv
```

**Debian, Ubuntu** and **Fedora, RHEL, openSUSE**: each
[GitHub release](https://github.com/mwala-zm/gitenv/releases/latest) has a
`.deb` and an `.rpm` for x86_64 and arm64. Download the one for your machine
and hand it to your package manager:

```sh
sudo apt install ./gitenv_<version>_amd64.deb
sudo dnf install ./gitenv-<version>.x86_64.rpm
```

<!-- Arch Linux: uncomment once the release workflow has AUR_SSH_PRIVATE_KEY
and gitenv-bin is on the AUR.

**Arch Linux**, from the AUR:

```sh
yay -S gitenv-bin
```
-->

**Cargo**, from crates.io, or prebuilt with
[cargo-binstall](https://github.com/cargo-bins/cargo-binstall):

```sh
cargo install gitenv
cargo binstall gitenv
```

**By hand**: prebuilt archives for macOS and Linux are attached to each
release. Unpack one and put `gitenv` somewhere on your `PATH`.

## Quick start

1. Run `gitenv` inside a repository. The first run creates a profile from
   your global git identity. Press `n` to add one for each client.
2. Give each profile the repositories it owns, by remote or by folder. In
   the form these are the remote and path rule fields, for example
   `github.com/acme/*` and `~/work/acme/**`.
3. Let git apply those rules by itself, so new clones start out right:

   ```sh
   gitenv export --install
   ```

4. Have every commit and push checked:

   ```sh
   gitenv hook install --global
   ```

5. Bring the clones you already have in line, and check your keys:

   ```sh
   gitenv scan ~/work --fix
   gitenv doctor
   ```

## The terminal UI

Run `gitenv` inside a repository, or point it at one: `gitenv ~/work/acme/api`.
Pointed at a folder that is not itself a repository, such as `gitenv ~/work`,
it opens on the list of repositories in it.

The left pane lists your profiles: `●` marks the one git is using here and
`◆` the one a rule says this repository belongs to. The right pane shows the
repository, the identity git will use, where that identity comes from,
whether it is right, and whether any of the last 20 commits were made as
someone else.

| Key | Action |
|---|---|
| `↑` `↓` or `j` `k` | move between profiles |
| `Enter` | write the selected profile into this repo's config |
| `e` | edit the selected profile |
| `n` | create a new profile |
| `d` | delete the selected profile (asks first) |
| `r` | re-read the repo and the profiles file |
| `s` | list every repository next to this one |
| `?` or `h` | show help |
| `q` or `Esc` | quit |

When a rule claims the repository, its profile is selected on start, so
`Enter` is the fix. Applying a profile writes `user.name`, `user.email` and
whatever else the profile sets into the repository's local config, which
always wins over your global config, so nothing global changes.

In the form, `Tab`/`↓` and `Shift-Tab`/`↑` move between fields, `←` `→`
`Home` `End` move within one, and `Ctrl-A`/`Ctrl-E`, `Ctrl-U`/`Ctrl-K`
(delete to start or end) and `Ctrl-W` (delete the word before the cursor)
work as in a shell.

In the repository list (`s`), `Enter` applies the profile a rule claims for
the highlighted repository, `o` opens it in the main view, `r` rescans and
`Esc` goes back.

## Profiles

Profiles live in one TOML file:

| Platform | Config path |
|---|---|
| macOS | `~/Library/Application Support/gitenv/config.toml` |
| Linux | `~/.config/gitenv/config.toml` |
| Windows (untested, build from source) | `%APPDATA%\gitenv\config\config.toml` |

Set `GITENV_CONFIG=/path/to/config.toml` to use another file, for example
one in a dotfiles repository; a symlinked config works too.

```toml
[[profiles]]
name = "personal"
user_name = "Ada Lovelace"
email = "ada@example.com"

[[profiles]]
name = "acme"
user_name = "Ada Lovelace"
email = "ada@acme.example"
signing_key = "ABCD1234"
ssh_key = "~/.ssh/id_acme"
remotes = ["github.com/acme/*"]
paths = ["~/work/acme/**"]

[[profiles]]
name = "oss"
user_name = "Ada Lovelace"
email = "ada@example.com"
signing_key = "~/.ssh/id_ed25519.pub"
signing_format = "ssh"
sign_commits = true

[profiles.git]
pull.rebase = true
"url.git@github.com:ada/.insteadOf" = "https://github.com/ada/"
```

Everything after `email` is optional:

- `signing_key` becomes `user.signingkey`. With `signing_format = "ssh"` it
  is the path to a public key.
- `signing_format` becomes `gpg.format` (`openpgp` or `gpg`, `ssh` or
  `x509`), and `sign_commits` (`true`, `false`, `yes` or `no`) becomes
  `commit.gpgsign`.
- `ssh_key` makes pushes and fetches from the repository use
  `ssh -i <key> -o IdentitiesOnly=yes`, so the right key is offered even
  with several loaded in your agent. `~` is expanded and spaces are fine.
- `remotes` and `paths` are [rules](#rules).
- `[profiles.git]` holds any other git setting the identity needs. Dotted
  keys can be written bare (`pull.rebase = true`) or quoted.

Every setting a profile has counts when gitenv decides whether a repository
is on it: the right email with a different signing key or SSH key is not a
match. When a profile leaves out the signing key, signing format,
`sign_commits` or `ssh_key`, applying it removes those settings from the
repository's own config, so a previous profile's cannot linger; your global
settings then apply.

Applying a profile replaces the `[profiles.git]` keys the previous one wrote
and removes those it does not set. A value you had set yourself before
gitenv first wrote that key is kept and put back once no profile sets it.
gitenv tracks this under `gitenv.managed` and `gitenv.backup` in the
repository's config.

You can edit the file by hand at any time. Unknown keys are an error, so a
typo such as `remote` for `remotes` is reported instead of silently ignored;
since `gitenv check` cannot look then, the hooks block commits until the
file is fixed. Saving from the TUI keeps your comments, layout and line
endings, and writes into the same file, so its permissions, owner, ACLs and
hard links stay. If the file changed on disk while the TUI was open, gitenv
refuses to save over it; press `r` to reload.

## Rules

Without rules, gitenv is content as long as a repository's identity is *one
of* your profiles. Rules say *which* profile a repository belongs to, so
gitenv can tell you it is on the wrong one.

`remotes` are globs matched against the remote URL reduced to
`host/owner/repo`. `github.com/acme/*` matches
`git@github.com:acme/api.git`, `https://github.com/acme/api` and
`https://user:token@github.com/acme/api.git` alike, ignoring case. `*`
stops at a slash and `**` does not, so `gitlab.com/acme/**` covers
subgroups, and a rule that is only a host, such as `github.com`, claims
everything on it. The `origin` remote decides when there is one, and
otherwise every remote is tried. `url.<base>.insteadOf` rewrites are applied first,
as git applies them.

`paths` are globs matched against the repository's top folder. They must
start with `/` or `~/`; `dir/**` also matches `dir` itself, and a folder
reached through a symlink matches either way.

When several rules match, the most specific one wins, wherever its profile
sits in the file: a remote rule beats a path rule, then the rule with the
longer text before its first wildcard wins, then the earlier profile. So
`~/projects/**` can belong to a personal profile and
`~/projects/clients/agency/**` to the agency's.

Where the identity comes from matters too. gitenv reads the repository's
local and worktree config over your global config, and, like git, lets
`GIT_AUTHOR_NAME`, `GIT_AUTHOR_EMAIL` and `EMAIL` override them. git sets
the author variables for hooks, so the pre-commit check sees the real
author of the commit being made, even one given with `--author`. Emails are
compared ignoring case.

## Keeping repositories right automatically

### Let git switch by itself

```sh
gitenv export --install
```

`gitenv export` writes each profile that has rules to a small git config
file, plus an `includes.gitconfig` that loads them with git's own
conditional includes: path rules become `includeIf "gitdir/i:..."` and
remote rules `includeIf "hasconfig:remote.*.url:..."` (git 2.36 or newer),
spelled out for every URL form and made case-insensitive, since git compares
remote URLs exactly. `--install` adds the file to the end of your global git
config, after any settings that would otherwise override it. From then on a
fresh clone has the right identity before its first commit, with no hook and
without gitenv running. When several rules match, the most specific one
wins, as in gitenv.

The files live in a `gitenv-includes` folder next to the profiles file, and
the TUI rewrites them whenever you save or delete a profile. A setting in a
repository's own config, such as one `gitenv apply` wrote, still wins.

Where git and gitenv can differ:

- git keeps every setting from every matching include. Where a broader
  rule of another profile can match the same repositories, a profile that
  leaves out `gpg.format`, `commit.gpgsign` or `core.sshCommand` gets the
  value from your global config (or git's default) written into its file, so
  the broader profile's does not apply. A `user.signingkey` or
  `[profiles.git]` key from the broader profile still applies if the
  narrower one does not set its own. Export again after changing those
  global settings.
- git checks every remote; gitenv prefers `origin` when there is one.
- git matches remote rules against the URL as written, before
  `url.<base>.insteadOf` rewrites; gitenv matches the rewritten URL.
- In a linked worktree or a submodule, git matches path rules against the
  main repository's `.git` folder; gitenv matches the checkout's own folder.

### Hooks

```sh
gitenv hook install            # this repository
gitenv hook install --global   # every repository
gitenv hook remove [--global]  # take them out again
```

The hooks run `gitenv check` before every commit and push. On a mismatch
the commit is refused and gitenv prints what git would have used and how to
fix it. Because git passes the commit's author to hooks, a wrong author
given with `--author`, kept by `--amend` or set in `GIT_AUTHOR_*` is caught
too.

`--global` uses your global `core.hooksPath`, or sets it to `~/.githooks`.
git ignores each repository's own `.git/hooks` once that is set, so gitenv
also writes a hook for every other event git runs hooks for, each handing
over to the repository's own hook of that name: `commit-msg`,
`post-checkout`, Git LFS and the rest keep working. Left out are
`reference-transaction` and `post-index-change`, which run on nearly every
git command, and `push-to-checkout`, whose presence alone changes how git
updates a repository you push to. With a global `core.hooksPath` set,
`gitenv hook install` and `remove` without `--global` refuse, since a hook
for one repository would not run there.

`remove --global` unsets `core.hooksPath` again if it is `~/.githooks` and
nothing else is left in it. When something is, such as a hook you edited,
the setting has to stay, so gitenv takes the identity check out but keeps
the hooks that run each repository's own, and says which files to remove.

A hook gitenv did not write, or one you edited after gitenv wrote it, is
never replaced or removed. If gitenv is not on the `PATH` a hook runs with
(some GUI clients use a short one), the hook warns and lets the commit
through rather than blocking every commit.

To write the hook yourself instead:

```sh
#!/bin/sh
# .git/hooks/pre-commit
exec gitenv check
```

## Command line

```sh
gitenv check             # exit code only: 0 right, 1 wrong, 2 could not look
gitenv status            # what git would use here, and whether it is right
gitenv status --short    # one token for a shell prompt, e.g. `acme`
gitenv apply acme        # write a profile into this repo without the UI
gitenv apply             # apply whichever profile's rules claim this repo
gitenv scan ~/work       # every repository under a folder, and its verdict
gitenv export --install  # let git apply your rules itself
gitenv hook install      # check every commit and push
gitenv doctor            # check every profile's keys exist and log in
gitenv fix-authors       # re-author unpushed commits made as someone else
gitenv completions zsh   # shell completion script
```

### Scripts and prompts

`gitenv check` prints nothing when all is well, and on a mismatch explains
what git would use and which profile the repository should be on. It exits
2 when it cannot look: not a repository, an unreadable config, no git.

`gitenv apply <profile>` does what `Enter` does in the UI, and `gitenv
apply` with no name applies the profile whose rules claim the repository.
If something stronger than the repository's config, such as worktree config
or `GIT_AUTHOR_*`, still makes git commit as someone else, it says so and
exits 1. Both take `--path DIR`, which makes a clone wrapper a one-liner:

```sh
git clone git@github.com:acme/api.git && gitenv apply --path api
```

`gitenv status` prints the repository, remote, identity, where the identity
comes from, recent commit authors, the matching profile, the rule that
applies and a verdict. `gitenv status --short` prints one token and exits
like `check`, so a prompt can colour it: `acme` when all is well,
`personal -> acme` when a rule says another profile should be in use,
`? -> acme` when the identity is no profile at all but a rule names one, and
`?` when nothing matches. Passwords in remote URLs are shown as `***`. For [Starship](https://starship.rs):

```toml
[custom.gitenv]
command = "gitenv status --short"
when = "git rev-parse --is-inside-work-tree"
format = "[$output]($style) "
style = "yellow"
```

### Auditing many repositories

`gitenv scan DIR` lists every repository under a folder with its verdict:

```
STATUS PROFILE          REPO
ok     acme             ~/work/acme/api
wrong  personal -> acme ~/work/acme/web
none   ?                ~/play/thing

3 repositories, 1 on the right profile, 2 to look at
```

`--problems` hides the `ok` rows. `--fix` applies the expected profile to
every repository a rule claims that is on the wrong identity, then reads it
again; one that something stronger still overrides, such as worktree config,
is reported and stays `wrong`. Repositories git cannot read are named on
stderr. The exit code is 1 if anything still needs attention. Hidden
folders, `node_modules` and `target` are skipped, and a repository's own
subfolders are not searched.

### Checking the profiles themselves

A repository can carry the right email and still push as the wrong account.
`gitenv doctor` checks that each profile would work:

```
acme <ada@acme.example>
  ok    ssh key /Users/ada/.ssh/id_acme exists
  ok    gpg has the secret key ABCD1234
  ok    github.com: Hi ada-acme! You've successfully authenticated, but GitHub does not provide shell access.
personal <ada@example.com>
  ok    nothing to check beyond name and email
```

It reports missing SSH and signing key files, a GPG key missing from your
keyring, `sign_commits` without a key, profiles with the same name or the
same identity, patterns that are not valid globs, relative path rules,
names git would trim when writing a commit, and `[profiles.git]` keys git
would reject. For each host a profile's remote
rules name, it runs `ssh -T` with only that profile's key and shows which
account the host greets, so a key registered to the wrong account stands
out. `--offline` skips that. It exits 1 if any check failed.

### Fixing commits made as someone else

Switching identity only protects the next commit. The TUI and `gitenv
status` look at the authors of the last 20 commits and warn when some were
made with another email than the repository should use.

`gitenv fix-authors` repairs the ones that are not published yet: commits
on the current branch that no remote branch, tag or other local branch has,
whose author or committer is not the right profile. It lists them, asks,
rebuilds them with the right author, keeping each commit's date, message,
content and other headers exactly, and then applies the profile. It never
touches your work tree, index or ignored files, runs no hooks, ignores
`git replace` objects, and moves the branch only if it is still where it was
when you were asked, so a commit or branch switch made meanwhile makes it
refuse. The old commits stay reachable through `git reflog`; notes stay on
them.

It refuses during a rebase, merge, cherry-pick, revert or bisect; when the
branch tracks a remote branch this clone has no copy of, as in a
single-branch clone, since those commits may well be pushed; when the
commits include a merge; at a shallow clone's cut-off; and with old-style
`.git/info/grafts`. Commits pushed by URL, with no remote-tracking branch to
show for it, look unpushed. For commits that are already pushed, use
`git filter-repo` and coordinate a force push.

### Shell completions

Homebrew installs them for you. Elsewhere, generate the script for your
shell and put it where the shell looks:

```sh
gitenv completions zsh  > ~/.zfunc/_gitenv            # zsh (fpath must include ~/.zfunc)
gitenv completions bash > ~/.local/share/bash-completion/completions/gitenv
gitenv completions fish > ~/.config/fish/completions/gitenv.fish
```

## Contributing

Bug reports, ideas and pull requests are welcome. For a bug, open an
[issue](https://github.com/mwala-zm/gitenv/issues) with what you ran, what
happened and what you expected, plus the output of `gitenv status` and
`git --version`; take any tokens or private emails out first. For a larger
change, open an issue to talk it through before writing the code.

### Setting up

You need Rust 1.88 or newer and git. Then:

```sh
git clone https://github.com/mwala-zm/gitenv.git && cd gitenv
cargo run          # launch the TUI in this repo
cargo test         # the tests; many drive real git in throwaway repos
```

The tests never change your own git config or profiles: each one works in
a fresh folder under the system temp directory.

### Before opening a pull request

CI runs these on Linux and macOS, so run them first:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

- Add a test for every bug fix and new behaviour. Where git is involved,
  test against real git in a temporary repository rather than a mock.
- The TUI has snapshot tests (`insta`). After an intended visual change,
  review the new snapshots with `cargo insta review` (from
  `cargo install cargo-insta`), or accept them all
  with `INSTA_UPDATE=always cargo test`, and commit the `.snap` files.
- Comments are doc comments on the functions that need explaining, and
  nowhere else; say why, not what.
- Update this README when behaviour changes.
- Keep each commit to one change, with a subject in the imperative ("Let the
  most specific rule claim a repo") and a body that explains why.
- Open the pull request against `main`.

### How the code is laid out

- `src/main.rs` parses the command line and runs the TUI's draw, wait,
  update loop.
- `src/app.rs` holds the TUI's state and maps key presses to actions;
  `src/ui.rs` draws it; `src/form.rs` is the profile form.
- `src/profile.rs` loads and saves the profiles file.
- `src/git.rs` reads and writes git config by running `git`.
- `src/rules.rs` decides which profile a repository should be on, and
  `src/check.rs` turns that into a verdict.
- `src/cmd.rs` is `apply` and `status`; `src/scan.rs`, `src/export.rs`,
  `src/hook.rs`, `src/doctor.rs` and `src/fix.rs` are `scan`, `export`,
  `hook`, `doctor` and `fix-authors`.

## License

MIT. See [LICENSE](LICENSE).
