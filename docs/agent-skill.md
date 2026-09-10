# Agent skill: driving Arreo (for CLI harnesses — including this loop)

> Arreo's socket API is the product surface: the same verbs humans type,
> scripts call, and agents orchestrate with. This doc teaches any CLI harness
> how to drive it. All examples assume a running daemon (`arreo-server`) and
> use `--socket` explicitly; without it the CLI uses `$XDG_RUNTIME_DIR/arreo.sock`.

## Verbs (one line each)

```console
arreo panes                                  # list panes (id + alive)
arreo spawn <id> <program> [args...]         # spawn a pane
arreo read <id> [--from N]                   # one-shot snapshot (no stream)
arreo attach <id>                            # stream deltas until exit (owns the connection)
arreo send <id> <text...>                    # type into a pane (add $'\r' to press Enter)
arreo wait <id> --state <s> [--timeout 5m]   # block until state (exit 0) or timeout (exit 1)
arreo split <id> <new-id>                    # sibling pane, same program
arreo metrics <id>                           # RSS/CPU/pids for one pane's tree
arreo server stop                            # graceful drain + exit 0
```

States: `working|idle|question|blocked|done|unknown`. Durations: `500ms`, `30s`, `5m`.

## The flagship loop (verified live this turn)

An agent that blocks on a question must never spin-poll. Instead:

```console
$ arreo spawn worker /bin/sh -c 'my-agent --task foo'
spawned worker
$ arreo wait worker --state question --timeout 5m
state=Question confidence=inferred:silence+prompt-shape pattern=Some("\\[y/n\\]")
$ arreo read worker
May I proceed? [y/n]
$ arreo send worker 'y' && arreo send worker $'\r'
$ arreo wait worker --state done --timeout 10m
state=Done confidence=direct:exit pattern=None
$ arreo read worker --from 40
```

Rules learned the hard way (T-0009 F4): **an attached connection is owned by
the stream** — open a fresh connection per verb (the CLI does this per
invocation). Never pipeline a second request onto an attached connection.

## Dogfood note (this loop uses these verbs)

The Arreo loop agent manages its own workers through these verbs where a
daemon is available: `spawn` a worker pane, `wait --state question` instead
of polling, `read` for context, `send` for answers, `metrics` to watch
budgets. Every example above was executed against the real daemon this turn
(see `.loop/evidence/T-0014/`), not written from the schema.
