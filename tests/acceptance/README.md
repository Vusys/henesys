# Acceptance test

`agent.sh` checks the product end to end: a real coding agent (`claude -p`) runs `npm test` through a henesys shim while the queue is full. It must wait, must not relaunch the command, and must get the result.

It is not run in CI. Run it by hand.

## Needs

- `claude` CLI, logged in
- `henesys` on `PATH` (for example `cargo build && export PATH="$PWD/target/debug:$PATH"`)
- `node`, `npm`, `jq`
- Linux for the `henesys-wait` process check (skipped elsewhere)

## Run

```sh
tests/acceptance/agent.sh
```

It prints `PASS` or `FAIL` per assertion and exits non-zero if any fail. It takes about a minute, and up to 5 minutes at most. It uses a private `HENESYS_DIR` and `HENESYS_SLOTS=2`, and prints its work directory, which holds `agent.out` and `proj/runs.log` for inspection.

If the script is killed partway, a waiter may stay behind. Kill only this run's waiter, found by its working directory (the printed work dir plus `/proj`). Do not kill all `henesys-wait` processes: other agents' queued commands are on the same box.
