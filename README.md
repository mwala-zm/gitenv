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
│                             ││identity is set in this repo's .git/config
└─────────────────────────────┘└────────────────────────────────────────┘
 ↑↓/jk select   Enter apply   e edit   n new   d delete   ? help   q quit
```

## Install

With a Rust toolchain:

```sh
cargo install gitenv
```

Prebuilt binaries for macOS and Linux are attached to each
[GitHub release](https://github.com/mwala-zm/gitenv/releases). Download the
archive for your platform, unpack it, and put `gitenv` somewhere on your
`PATH`. If you have [cargo-binstall](https://github.com/cargo-bins/cargo-binstall):

```sh
cargo binstall gitenv
```

gitenv shells out to `git`, so git must be installed.

## Use

Run it inside any repository, or point it at one:

```sh
cd ~/work/acme/api && gitenv
gitenv ~/work/acme/api
gitenv check             # no UI: just the exit code, for hooks and scripts
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

`signing_key` and `ssh_key` are optional. When `ssh_key` is set, pushes from
that repo use `ssh -i '<path>' -o IdentitiesOnly=yes`, so the right key is
offered even if several are loaded in your agent. A leading `~` is expanded,
and the path is quoted so spaces are fine. The ssh key counts towards the
match: a repo with the right email but a different `core.sshCommand` is
reported as not matching.

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
