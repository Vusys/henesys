# henesys

Makes heavy commands on a shared machine wait their turn, and says what they are waiting behind.

henesys is a foreground, blocking, first-come-first-served admission queue. The box has `S` slots; a command takes `N` of them. Output passes straight through to the caller. While a command waits, henesys prints to stderr what it is waiting behind:

```
[henesys] waiting 90s for 2 of 6 slots (5 busy), #2 in line. Running: web: npm run build (3 slots, 2m); api: npm test (2 slots, 14s). This is expected; it starts by itself. Do not re-run this command.
```

It is for boxes where several AI coding agents or CI jobs each run test suites and browser tests. Without a queue the load average sits at 15–35, checks take 22 seconds or more, callers' timeouts expire, and the false failures cause re-runs that add more load.

On a quiet box, nobody notices henesys is there.

## Install

**Arch / CachyOS / Manjaro (pacman):**

```
curl -LO https://github.com/Vusys/henesys/releases/latest/download/PKGBUILD && makepkg -si
```

The PKGBUILD pins the tarball's sha256, and pacman tracks the install (`pacman -Qi henesys`, `pacman -R henesys`). To upgrade, run the same line again; pacman does not see new versions by itself. For a pre-release, use `releases/download/<tag>/PKGBUILD`, because `latest` skips pre-releases.

**macOS (Homebrew):**

```
brew tap vusys/henesys https://github.com/Vusys/henesys
brew install henesys
```

**Other Linux:** download `henesys-<version>-<target>.tar.gz` and its `.sha256` from [Releases](https://github.com/Vusys/henesys/releases), check it with `shasum -a 256 -c henesys-<version>-<target>.tar.gz.sha256`, extract, and put `henesys` on your PATH.

**From source:**

```
cargo install --git https://github.com/Vusys/henesys
```

**Upgrades:** re-run the PKGBUILD line, or `brew upgrade henesys`. Shims are symlinks, so they follow upgrades automatically.

## Quick start

```
henesys run -w 2 -- npm test
henesys status
```

`-w 2` says the job needs two slots. `henesys status` lists the slots, who holds them, and who is waiting.

## How it works

**Slots.** The box has `S` slots. The default is the number of CPU cores minus 2, and at least 1. Override it with `HENESYS_SLOTS` or `slots = N` in the config file (`HENESYS_SLOTS` wins). The default uses `available_parallelism`, which respects cgroup CPU quotas.

**Weights.** `-w N` (default 1) is how many slots a command takes. A weight above `S` is clamped to `S`, so a big job on a small box still runs, alone.

**Strict FIFO.** Waiters are served in arrival order, and nothing jumps the head of the line. A 1-slot job behind a blocked 3-slot job waits, so big jobs never starve. The cost is some idle slots while the head waits. `-p` / `--priority` puts a waiter ahead of all normal waiters, in its own arrival order.

**Nesting.** A henesys run started inside another henesys-held command (`HENESYS_HELD` is set) runs at once and never waits. The outer command already holds its slots, so waiting for more would deadlock.

**Fast path.** When slots are free and nobody is ahead, a command starts immediately and prints nothing. `HENESYS_DISABLE=1` skips the queue entirely and runs the command at once.

**Liveness.** A running command holds an OS file lock for its lifetime. A crash or `kill -9` releases it automatically; there is no PID sweep. Children never inherit the lock.

**Waiting messages** go to stderr and always start with `[henesys] `. When stderr is a terminal, one line is redrawn in place; otherwise a line is printed every 30 seconds.

## Shims

A shim is a symlink named after a tool (`npm`, `npx`). When it runs, henesys reads its rules, decides whether the call needs the queue, and then runs the real tool found later on PATH.

```
henesys shim install ~/.local/share/henesys/shims
```

With no tool names, this installs a shim for every tool in your rules (the default rules cover `npm` and `npx`). Name tools to choose: `henesys shim install <dir> npm npx vitest`. An existing file that is not a henesys shim is never overwritten.

Put the directory first on PATH:

- bash / zsh: `export PATH="$HOME/.local/share/henesys/shims:$PATH"`
- fish: `fish_add_path -p ~/.local/share/henesys/shims`

Shims need no config for the default rules.

### Rules

Rules live in `~/.config/henesys/config.toml` (or `$XDG_CONFIG_HOME/henesys/config.toml`, or `HENESYS_CONFIG`). The first rule whose `tool`, `script`, `command` and `under` all match the call decides. If the file has any `[[rule]]` entries, they replace the built-in rules entirely.

| Field | Meaning |
|-------|---------|
| `tool` | Basename the shim is called as. Required. |
| `script` | Glob list for the npm-style script name (`npm run X`, `npm test`, `pnpm X`). Matches when the script name matches any glob. |
| `command` | Glob list for the first non-flag argument (`npx vitest`). |
| `under` | Rule applies only when the current directory is inside this path. `~/` expands to `$HOME`. |
| `weight` | Slots to take. Positive integer. Default 1. |
| `weight_from_flag` | Take the weight from this flag's value, `--workers 4` or `--workers=4`. Ignored if the value is not a positive integer. |
| `weight_if_arg` | Table of glob → weight. A glob that matches any argument sets the weight. |
| `passthrough` | Arguments that mean "do not queue"; the call runs at once. |

Globs: `*` matches any run of characters, including none, and the whole argument must match.

Unknown fields are an error, and the error names the rule number.

The built-in rules are:

```toml
[[rule]]
tool = "npm"
script = ["check*", "test*", "build*"]
weight = 2
passthrough = ["--help", "--watch"]

[[rule]]
tool = "npm"
script = ["e2e*"]
weight_from_flag = "--workers"
passthrough = ["--help", "--list"]

[[rule]]
tool = "npx"
command = ["vitest"]
weight = 2
weight_if_arg = { "*.test.ts" = 1 }
passthrough = ["--help", "--version", "--watch", "-w"]
```

So `npm test` takes 2 slots, `npm run lint` runs at once, `npm run e2e -- --workers 4` takes 4 slots, and `npx vitest run src/a.test.ts` takes 1.

A config that says `[[rule]]` with a rule for `cargo` replaces all of the above, so `npm` is no longer queued. Add `npm` rules too if you want them.

Example: queue only under one project directory:

```toml
[[rule]]
tool = "npm"
under = "~/code/big-app"
weight = 2
```

### Errors fall back to running

If the config file is malformed, or `slots` / `HENESYS_MAX_WAIT` are invalid, a shim prints the error and runs the tool without the queue:

```
henesys: /home/you/.config/henesys/config.toml: ...; running npm without the queue
```

The command still runs. `henesys run` reports the same error and exits 125 instead.

### Known limit: `npm` flags before the script name

`script_name` assumes npm's own flags take no value. `npm --prefix dir run test` finds no script name, so the rule does not match and the command runs without the queue. Put flags that take a value after the script name, or call the command through `henesys run -w N -- npm ...`.

### Watch mode holds slots

A long-lived command holds its slots until you quit it. The default rules pass `--watch` through, but a bare `npx vitest` in a terminal watches by default and is queued. Either add `passthrough = ["--watch", ...]` for it, or use `vitest run`.

## Choosing weights

The weight should match the job's real parallelism. Cap the tool's own workers to the same number, or it will use more cores than you queued for:

- vitest: `--maxWorkers`
- jest: `--maxWorkers`
- Playwright: `--workers`
- make: `make -j`
- cargo: `cargo build -j`

henesys is an admission queue. It does not clamp CPU. Weights are estimates; a job that uses more cores than its weight still runs, and the queue only controls how many jobs start at once.

To limit CPU too, compose with another tool (see [Limiting CPU](#limiting-cpu)).

## For agents and supervisors

- Every message goes to stderr and starts with `[henesys] `.
- A waiting command is queued, not stuck. Paste this into agent instructions:

  > If a command prints `[henesys] waiting`, it is queued, not stuck. Wait for it. Do not re-run or kill it.

- To detect waiters: on Linux, `pgrep -x henesys-wait`; anywhere, `henesys status --json | jq '.waiters[].pid'`.
- `henesys status --json` is the machine-readable view. See [Reference](#reference).

## Timeouts

**Waiting timeout.** `--max-wait SECS` (or `HENESYS_MAX_WAIT`) gives up after that many seconds of waiting. The command does not run and henesys exits 75, which means "busy, try later", not "failed". Set it below your caller's own timeout, so the caller sees the 75 instead of timing out.

```
henesys run --max-wait 600 -- npm test
```

**Run-time limit.** henesys has no `--max-time`. Compose with `timeout(1)`:

```
henesys run -- timeout 1h npm test
```

GNU `timeout` signals its whole process group, so grandchild processes (test workers, dev servers) are killed too. This is tested on Linux (`timeout_kills_grandchildren` in `tests/run.rs`). On macOS, `timeout` comes from `brew install coreutils` as `gtimeout`. I did not verify grandchild kill with `gtimeout`.

## Limiting CPU

This is optional and by composition. henesys does not do it itself.

**Linux**, with systemd user sessions:

```
henesys run -w 4 -- systemd-run --user --scope -p AllowedCPUs=0-3 -- npm run build
henesys run -w 4 -- systemd-run --user --scope -p CPUQuota=400% -- npm run build
```

**macOS:**

```
henesys run -w 4 -- taskpolicy -c utility npm run build
```

`taskpolicy` lowers the job's QoS class rather than enforcing a hard cap.

**Either OS:**

```
henesys run -w 2 -- nice -n 10 npm run build
```

## Sharing a budget between Unix users

Create the shared directory with group write and the setgid bit, then point `HENESYS_DIR` at it. Every user of the budget must be in the same group.

```
sudo install -d -m 2770 -g <group> /srv/henesys
```

henesys creates `wait`, `held`, `.mutex` and `VERSION` inside it and gives them the directory's group permissions, so your umask does not matter. Each user sets `HENESYS_DIR=/srv/henesys`.

## Reference

### Commands

```
henesys run [-w N] [-l TEXT] [-p] [--max-wait SECS] [--] <cmd> [args...]
henesys status [--json]
henesys shim install <dir> [tool...]
henesys --version
henesys --help
```

- `run -w N` weight, default 1. `-l TEXT` label shown in messages, default `<cwd basename>: <command>`, truncated to 80 characters. `-p` priority. `--max-wait SECS` waiting timeout. Everything after `--` is the command and its arguments.
- `status` prints the slot count, busy slots, waiters, and each holder and waiter. `--json` prints the schema below.
- `shim install <dir> [tool...]` creates symlinks in `<dir>` to the henesys binary. Without tool names, it uses the tools in your rules.

### Environment

| Variable | Meaning |
|----------|---------|
| `HENESYS_SLOTS` | Slot budget `S`. Positive integer. Overrides `slots` in the config. |
| `HENESYS_DIR` | State directory. Default `$XDG_RUNTIME_DIR/henesys`, else `/tmp/henesys-<uid>`. |
| `HENESYS_CONFIG` | Config file path. Default `$XDG_CONFIG_HOME/henesys/config.toml`, else `~/.config/henesys/config.toml`. |
| `HENESYS_HELD` | Set to `1` by henesys in every command it queues. A henesys run inside it does not wait. Do not set it yourself. |
| `HENESYS_DISABLE` | Set to `1` to skip the queue. Any value other than empty or `0` counts. |
| `HENESYS_MAX_WAIT` | Default `--max-wait`, in seconds. |

### Exit codes

| Code | Meaning |
|------|---------|
| the child's own code | The command ran and exited with it. |
| `128 + signal` | The command was killed by a signal. For example `kill -9` gives 137. |
| `75` | henesys gave up waiting (`--max-wait`). The command did not run. |
| `125` | henesys error: bad usage, bad state, or bad config in `run`. |
| `126` | The command exists but is not executable. |
| `127` | The command was not found. |

### `status --json`

```json
{
  "version": 1,
  "dir": "/run/user/1000/henesys",
  "slots": 6,
  "busy": 5,
  "holders": [
    {"pid": 1234, "slots": 3, "label": "web: npm run build", "cwd": "/home/me/web", "since": 1760000000, "age_s": 120}
  ],
  "waiters": [
    {"pid": 1235, "slots": 2, "label": "api: npm test", "cwd": "/home/me/api", "since": 1760000100, "waited_s": 20, "position": 1, "priority": false}
  ]
}
```

`since` is Unix time in seconds. `position` is 1 for the head of the line. Waiters are listed in queue order. `version` is 1.

### State directory

```
<dir>/
  VERSION     protocol version, currently "1"
  .mutex      lock held for each read or change of wait/ and held/
  wait/       one file per waiting command
  held/       one file per running command
```

A henesys whose protocol does not match `VERSION` refuses to run against that directory. Use a matching henesys or a different `HENESYS_DIR`.

### Stability

From 1.0, these are stable and follow semver: the state directory layout and protocol version, the `[henesys] ` message prefix and wording, the exit codes above, the environment variables above, the config and rules schema, and the `status --json` schema.

## Limitations

- Linux and macOS only.
- Ctrl-Z does not suspend the job from the shell's point of view. The job keeps its slots.
- `kill -9` of henesys orphans its child, which keeps running. The slots are released.
- npm flags that take a value before the script name are misread (see [Shims](#known-limit-npm-flags-before-the-script-name)).
- Weights are estimates.

## Licence

MIT. See [LICENSE](LICENSE).
