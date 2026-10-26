# gitenv

A small terminal UI that makes sure you commit with the right git identity
for the repo you're in.

If you work with more than one team, you have probably pushed a commit with
the wrong name, email, or signing key at least once. gitenv shows you, at a
glance, which identity git will use in the current repo, whether it matches
one of your saved profiles, and lets you switch with one key press.

```
 gitenv  right identity, right repo
┌ Profiles ───────────────────┐┌ Current repository ───────────────────┐
│  personal  me@example.com   ││root        ~/work/acme/api             │
│> ● acme    me@acme.example  ││remote      git@github.com:acme/api.git │
│                             ││                                        │
│                             ││Effective identity                      │
│                             ││name        Ada Lovelace                │
│                             ││email       me@acme.example             │
│                             ││signing key ABCD1234                    │
│                             ││                                        │
│                             ││✔ matches profile 'acme'                │
│                             ││rule: 'acme' claims this repo (remote matches github.com/acme/*)
│                             ││identity is set in this repo's .git/config
└─────────────────────────────┘└────────────────────────────────────────┘
 ↑↓/jk select   Enter apply   e edit   n new   d delete   ? help   q quit
```

## Install

gitenv shells out to `git`, so git must be installed.

**Homebrew** (macOS and Linux):

```sh
brew tap mwala-zm/tap
brew install gitenv
```

Or in one step: `brew install mwala-zm/tap/gitenv`.

**Debian, Ubuntu** and **Fedora, RHEL, openSUSE**: each
[GitHub release](https://github.com/mwala-zm/gitenv/releases/latest) has a
`.deb` and an `.rpm` for x86_64 and arm64. Download the one for your machine
and hand it to your package manager:

```sh
sudo apt install ./gitenv_<version>_amd64.deb
sudo dnf install ./gitenv-<version>.x86_64.rpm
```

**Arch Linux**, from the AUR:

```sh
yay -S gitenv-bin
```

**Cargo**, from crates.io, or prebuilt with
[cargo-binstall](https://github.com/cargo-bins/cargo-binstall):

```sh
cargo install gitenv
cargo binstall gitenv
```

**By hand**: prebuilt archives for macOS and Linux are attached to each
release. Unpack one and put `gitenv` somewhere on your `PATH`.

## Use

Run it inside any repository, or point it at one:

```sh
cd ~/work/acme/api && gitenv
gitenv ~/work/acme/api
gitenv check             # no UI: just the exit code, for hooks and scripts
gitenv status            # what git would use here, and whether it is right
gitenv status --short    # one token for a shell prompt, e.g. `acme`
gitenv apply acme        # write a profile into .git/config without the UI
gitenv apply             # apply whichever profile's rules claim this repo
gitenv scan ~/work       # every repo under a directory, and its verdict
gitenv export --install  # let git apply your rules itself, even in new clones
gitenv doctor            # check every profile's keys exist and log in
```

| Key | Action |
|---|---|
| `↑` `↓` or `j` `k` | move between profiles |
| `Enter` | write the selected profile into this repo's `.git/config` |
| `e` | edit the selected profile |
| `n` | create a new profile |
| `d` | delete the selected profile (asks first) |
| `r` | re-read the repo |
| `?` or `h` | show help |
| `q` or `Esc` | quit |

Applying a profile sets `user.name`, `user.email`, and optionally
`user.signingkey`, `gpg.format`, `commit.gpgsign` and `core.sshCommand` in
the repo's local config. Local
config always wins over `~/.gitconfig`, so the global identity is left alone.

### Scripts and prompts

`gitenv apply <profile>` does what Enter does in the UI, and `gitenv apply`
with no name applies the profile whose [rules](#rules) claim the repo. Both
take `--path DIR` to act on a repo you are not inside. That makes a
post-checkout hook or a clone wrapper a one-liner:

```sh
git clone git@github.com:acme/api.git && gitenv apply --path api
```

`gitenv status` prints the repo, remote, identity, matching profile, the
rule that applies and a verdict. `gitenv status --short` prints one token
and exits like `check`, so a prompt can colour it: `acme` when all is well,
`personal -> acme` when a rule says another profile should be in use, `?`
when nothing matches. For [Starship](https://starship.rs):

```toml
[custom.gitenv]
command = "gitenv status --short"
when = "git rev-parse --is-inside-work-tree"
format = "[$output]($style) "
style = "yellow"
```

### Auditing many repositories

`gitenv scan DIR` finds every repository under a directory and prints one
line per repo with its verdict, then a summary:

```
STATUS PROFILE          REPO
ok     acme             ~/work/acme/api
wrong  personal -> acme ~/work/acme/web
none   ?                ~/play/thing

3 repositories, 1 on the right profile, 2 to look at
```

`--problems` hides the `ok` rows. `--fix` applies the expected profile to
every repo a rule claims that is on the wrong identity, so after adding a
rule you can bring all your clones in line at once. The exit code is 1 if
anything still needs attention. Hidden directories, `node_modules` and
`target` are skipped, and a repo's subdirectories are not searched.

### Checking the profiles themselves

A repo can carry the right email and still push with the wrong account.
`gitenv doctor` checks that each profile would work:

```
acme <ada@acme.example>
  ok    ssh key /Users/ada/.ssh/id_acme exists
  ok    gpg has the secret key ABCD1234
  ok    github.com: Hi ada-acme! You've successfully authenticated, but GitHub does not provide shell access.
personal <ada@example.com>
  ok    nothing to check beyond name and email
```

It reports missing ssh and signing key files, a GPG key missing from your
keyring, `sign_commits` without a key, two profiles with the same name or
the same identity, and patterns that are not valid globs. For every host a
profile's remote rules name, it runs `ssh -T` with only that profile's key
and shows the account the host greets, so a key registered to the wrong
account stands out. `--offline` skips that step. It exits 1 if any check
failed.

### Shell completions

Homebrew installs them for you. Elsewhere, generate the script for your
shell and put it where the shell looks:

```sh
gitenv completions zsh  > ~/.zfunc/_gitenv            # zsh (fpath must include ~/.zfunc)
gitenv completions bash > ~/.local/share/bash-completion/completions/gitenv
gitenv completions fish > ~/.config/fish/completions/gitenv.fish
```

### Catching past mistakes

Switching identity only protects the next commit. The repository pane and
`gitenv status` also look at the authors of the last 20 commits on the
current branch and warn when some were made with a different email than
the repo should be using (the profile a rule claims it for, else the one
git would use now), with a count per author. Use `git rebase` with
`--reset-author` or `git filter-repo` to repair those before pushing.

## Hooks

`gitenv check` exits 0 when the repo's identity matches a saved profile, 1
when it does not, and 2 when it could not look (not a repo, config
unreadable, git missing). It prints nothing on success, so it makes a quiet
hook. gitenv installs it as a pre-commit and a pre-push hook for you:

```sh
gitenv hook install            # this repository
gitenv hook install --global   # every repository, through core.hooksPath
gitenv hook remove [--global]  # take them out again
```

A hook gitenv did not write is never replaced; you are told which line to
add to it instead. `--global` uses your global `core.hooksPath`, or sets it
to `~/.githooks`. git skips each repo's own `.git/hooks` once that is set,
so the global hooks run the repo's own hook after the check, and `remove
--global` unsets `core.hooksPath` again if gitenv set it. If gitenv is not
on `PATH` when a hook runs (some GUI clients use a short one), the hook
warns and lets the commit through rather than blocking every commit.

To write the hook yourself instead:

```sh
#!/bin/sh
# .git/hooks/pre-commit
exec gitenv check
```

On a mismatch the commit is refused and gitenv prints what git would have
used and which repo to open it in.

## Profiles

On first run gitenv creates a config file seeded from your global git
identity. Press `n` to add a profile or `e` to change one from inside the
TUI, or edit the file directly with one `[[profiles]]` block per identity.
In the form, `Tab`/`↓` and `Shift-Tab`/`↑` move between fields, `←` `→`
`Home` `End` move within one, and `Ctrl-A`/`Ctrl-E`, `Ctrl-U`/`Ctrl-K`
(delete to start/end) and `Ctrl-W` (delete the word before the cursor)
work as in a shell.

| Platform | Config path |
|---|---|
| macOS | `~/Library/Application Support/gitenv/config.toml` |
| Linux | `~/.config/gitenv/config.toml` |
| Windows | `%APPDATA%\gitenv\config\config.toml` |

Set `GITENV_CONFIG=/path/to/config.toml` to use a different file, for
example one kept in a dotfiles repo.

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
ssh_key = "/Users/ada/.ssh/id_acme"

[[profiles]]
name = "oss"
user_name = "Ada Lovelace"
email = "ada@example.com"
signing_key = "~/.ssh/id_ed25519.pub"
signing_format = "ssh"
sign_commits = true
```

Everything after `email` is optional, and the `[profiles.git]` table can
hold any other key the identity needs:

```toml
[[profiles]]
name = "acme"
user_name = "Ada Lovelace"
email = "ada@acme.example"

[profiles.git]
"url.git@github.com:acme/.insteadOf" = "https://github.com/acme/"
pull.rebase = true
```

Those keys are written with the profile and checked when matching. gitenv
notes the keys it wrote under `gitenv.managed` in the repo's config, so
applying another profile removes the ones that profile does not set; keys
you set by hand are left alone. Dotted keys can be written bare
(`pull.rebase = true`) or quoted (`"pull.rebase" = "true"`). `signing_format` is written to
`gpg.format` (`openpgp`, `ssh` or `x509`) and `sign_commits` to
`commit.gpgsign`; with SSH signing, `signing_key` is the path to the public
key. Both count towards the match, so a repo that would make unsigned or
GPG-signed commits when the profile asks for SSH-signed ones is reported as
not matching. Leaving them out leaves git's own setting alone. When `ssh_key` is set, pushes from
that repo use `ssh -i '<path>' -o IdentitiesOnly=yes`, so the right key is
offered even if several are loaded in your agent. A leading `~` is expanded,
and the path is quoted so spaces are fine. The ssh key counts towards the
match: a repo with the right email but a different `core.sshCommand` is
reported as not matching.

## Rules

Without rules, gitenv is happy as long as the repo's identity is *one of*
your profiles. Rules let a profile claim repositories, so gitenv can tell
you the repo is on the *wrong* profile:

```toml
[[profiles]]
name = "acme"
user_name = "Ada Lovelace"
email = "ada@acme.example"
remotes = ["github.com/acme/*", "gitlab.acme.example/**"]
paths = ["~/work/acme/**"]
```

`remotes` are globs matched against the origin URL reduced to
`host/owner/repo`, so `github.com/acme/*` matches
`git@github.com:acme/api.git` and `https://github.com/acme/api.git` alike.
`*` stops at a slash and `**` does not. `paths` are globs matched against
the repository root; `~` is your home directory and `dir/**` also matches
`dir` itself.

When several rules match, the most specific one wins, wherever its profile
sits in the file: a remote rule beats a path rule, and otherwise the rule
with the longer text before its first wildcard wins. So a freelancer can
give `~/projects/**` to a personal profile and
`~/projects/clients/agency/**` to the agency's, and repos under `agency`
belong to the agency. Rules that are equally specific fall back to file
order.

When a rule claims the repo, the TUI marks that profile with `◆`, highlights
it on start so Enter applies it, and the repository pane says which profile
the repo is on and which it should be on. `gitenv check` exits 1 with the
same explanation, even if the current identity is another saved profile.

### Letting git switch by itself

Rules tell you a repo is on the wrong profile; `gitenv export` makes git
pick the right one in the first place. It writes each profile that has
rules to a small git config file and an `includes.gitconfig` that loads
them with git's own conditional includes:

```sh
gitenv export --install
```

Path rules become `includeIf "gitdir/i:..."` and remote rules
`includeIf "hasconfig:remote.*.url:..."` (git 2.36 or newer), spelled out
for SSH and HTTPS URLs since git compares them as written. `--install` adds
one `include.path` line to your global config. From then on a fresh clone
has the right name, email, signing key and SSH key before its first
commit, with no hook and without gitenv running. When several rules match,
the most specific one wins, as it does in gitenv.

The files live in a `gitenv-includes` directory next to the profiles file,
and the TUI rewrites them whenever you save or delete a profile. Anything
`gitenv apply` writes to a repo's `.git/config` still takes precedence,
since local config always wins. One difference from gitenv's own matching:
git checks every remote, while gitenv prefers `origin` when there is one.

## Development

```sh
cargo run          # launch the TUI in this repo
cargo test         # unit tests, including one that inits a throwaway repo
cargo clippy       # lints
```

The code is small and split by responsibility:

- `src/main.rs` sets up the terminal and runs the draw, wait, update loop.
- `src/app.rs` holds all state and maps key presses to actions.
- `src/check.rs` is the `check` subcommand used from hooks.
- `src/form.rs` is the profile editor form and its validation.
- `src/ui.rs` draws the screen from that state and never mutates it.
- `src/profile.rs` defines profiles and loads and saves the config file.
- `src/git.rs` reads and writes git config by shelling out to `git`.

## Roadmap

- Rules that pick a profile automatically from the remote URL or directory.

## License

MIT. See [LICENSE](LICENSE).
