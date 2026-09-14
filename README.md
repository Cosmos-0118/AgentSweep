# AgentSweep

Understand and control what your coding agents store locally.

Codex, Claude Code, Cursor, opencode, VS Code, Windsurf, Kiro, and Antigravity
all quietly accumulate logs, caches, session history, and shell snapshots on
disk. AgentSweep inventories all of it, tells you what's safe to delete and
what isn't, and cleans it — with a quarantine-and-restore safety net instead
of `rm -rf`.

## Install

**Homebrew (macOS/Linux):**

```bash
brew tap Cosmos-0118/agentsweep
brew install agentsweep
```

**From source:**

```bash
cargo install --path .
```

## Usage

Run `agentsweep` with no arguments to open the interactive dashboard, or use
the CLI directly:

```bash
agentsweep scan                     # inventory storage across all tools
agentsweep clean --safe             # delete only what's provably safe
agentsweep clean --smart --dry-run  # preview a wider, still-guarded cleanup
agentsweep restore --last           # undo the most recent clean
agentsweep optimize                 # get prevention advice for repeat offenders
```

Add `--tool <name>` to scope any command to one adapter (`codex`, `claude`,
`cursor`, `opencode`, `vscode`, `windsurf`, `kiro`, `antigravity`), `--json`
for machine-readable output, and `--plain` to disable color/animation.

### Dashboard keys

| Key | Action |
| --- | --- |
| `↑ ↓` | move within the item list |
| `← →` | switch tool |
| `tab` | cycle mode: SAFE · SMART · DEEP |
| `o` | cycle age filter: All · >7d · >30d · >90d |
| `space` | toggle item (locked rows show REFUSED) |
| `a` / `n` | select everything the mode allows / clear selection |
| `d` | clean selection |
| `r` | restore from quarantine |
| `p` | prevention / optimize |
| `?` | shortcuts |
| `q` / `esc` | quit / back |

## How cleaning works

Every deletable path is tagged with a risk tier, and each clean mode only
touches the tiers it's allowed to:

| Risk | Safe | Smart | Deep |
| --- | :-: | :-: | :-: |
| **Safe** — regenerable caches/logs | ✅ | ✅ | ✅ |
| **Review** — session/log data the tool can't regenerate | | ✅ | ✅ |
| **Userdata** — conversation history, attachments | | | ✅ |
| **Critical** — refused outright, never deletable | | | |

Anything above Safe goes through a hold-to-confirm gesture in the dashboard,
and non-Safe deletions land in a local quarantine first — `agentsweep
restore --last` (or `--id <id>`) brings them back. Quarantine snapshots are
garbage-collected after N days with `agentsweep restore --gc-days <N>`.

Rules live in [`rules/rules.toml`](rules/rules.toml) and declare, per tool,
which paths are safe, which require the tool to be stopped first, which have
an age threshold, and the exact consequence of deleting them.

## Development

```bash
cargo test              # unit + integration tests
cargo fmt --all          # format
cargo clippy --all-targets -- -D warnings   # lint (CI runs this too)
```

CI (`.github/workflows/ci.yml`) runs fmt, clippy, and tests on macOS and
Linux for every push and PR. Pushing a `v*` tag runs
[`release.yml`](.github/workflows/release.yml): it builds binaries for
macOS (arm64/x86_64) and Linux (x86_64), publishes a GitHub release, and
regenerates `homebrew/agentsweep.rb` with the new version, URLs, and SHA256
checksums, and syncs it to the [`homebrew-agentsweep`](https://github.com/Cosmos-0118/homebrew-agentsweep)
tap automatically.

## License

[MIT](LICENSE)
