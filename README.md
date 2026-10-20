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
`user.signingkey` and `core.sshCommand` in the repo's local config. Local
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

### Shell completions

Homebrew installs them for you. Elsewhere, generate the script for your
shell and put it where the shell looks:

```sh
gitenv completions zsh  > ~/.zfunc/_gitenv            # zsh (fpath must include ~/.zfunc)
gitenv completions bash > ~/.local/share/bash-completion/completions/gitenv
gitenv completions fish > ~/.config/fish/completions/gitenv.fish
```

## Hooks

`gitenv check` exits 0 when the repo's identity matches a saved profile, 1
when it does not, and 2 when it could not look (not a repo, config
unreadable, git missing). It prints nothing on success, so it makes a quiet
pre-commit hook:

```sh
#!/bin/sh
# .git/hooks/pre-commit
exec gitenv check
```

To guard every repository you have cloned, point git at one hooks directory
for all of them and put the script there instead:

```sh
git config --global core.hooksPath ~/.githooks
mkdir -p ~/.githooks && printf '#!/bin/sh\nexec gitenv check\n' > ~/.githooks/pre-commit
chmod +x ~/.githooks/pre-commit
```

On a mismatch the commit is refused and gitenv prints what git would have
used and which repo to open it in.

## Profiles

On first run gitenv creates a config file seeded from your global git
identity. Press `n` to add a profile or `e` to change one from inside the
TUI, or edit the file directly with one `[[profiles]]` block per identity.

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
```

`signing_key`, `ssh_key`, `remotes` and `paths` are optional. When `ssh_key` is set, pushes from
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
`dir` itself. The first profile with a matching rule wins.

When a rule claims the repo, the TUI marks that profile with `◆`, highlights
it on start so Enter applies it, and the repository pane says which profile
the repo is on and which it should be on. `gitenv check` exits 1 with the
same explanation, even if the current identity is another saved profile.

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
