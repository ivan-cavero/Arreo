# Themes — one theme, every surface (T-0016, T-0116)

> Operator's and implementer's reference for themes: the file format and the
> hierarchy themes are found in, the **wire shape** the daemon answers with, and
> the rule that makes one document render correctly on a truecolor terminal, a
> 16-colour one, a phone and (Phase 4) a browser. The engine is
> `crates/arreo-core/src/theme/`; the e2e slice is
> `cargo xtask e2e --slice theme`.

**One sentence: a theme is resolved once, by the machine that owns it, and what
travels is the resolved `token → color` table** — not the file, not the `defs`,
not the variant pairs — so every surface renders the same palette and each
surface applies only the one thing that is genuinely its own: how many colors its
terminal can show.

```console
arreo theme export [<name>] [--variant dark|light] [--json] [--socket PATH]
cargo xtask e2e --slice theme          # engine + a real pty at every depth + the wire
```

## 1. Where a theme comes from

Two JSON formats exist in this engine, and they are **not the same thing**:

| Format | Where | What it is |
| --- | --- | --- |
| `theme/schema.rs::RawTheme` | `*.json` theme files | The theme a user writes: `defs` (named colors), `theme` (per-token values, each a literal *or* a `defs` reference, optionally per variant). Validated at load; unknown tokens and dangling references are errors, never silence. |
| `theme/brand.rs` | `design/BRAND.md` | The brand document (§2's token list). A design artifact, not a user theme. |

Themes are discovered by name through the hierarchy
`ARREO_THEME_DIR` → `$XDG_CONFIG_HOME/arreo/themes` → `<project root>/.arreo/themes`
→ `./.arreo/themes`, later directories winning, with the built-ins (`arreo`,
`tokyonight`, `catppuccin`, `gruvbox`, `system`) embedded in the binary and always
present. A partial theme inherits the rest of the `arreo` base look (BRAND's
shipped palette).

**That hierarchy is a filesystem, and a phone does not have one** — which is what
the wire shape in §2 exists for.

## 2. The wire shape

Two socket ops (T-0116), both in `crates/arreo-core/src/proto/message.rs`:

| Op | Direction | Fields |
| --- | --- | --- |
| `theme` | client → server | `name` (empty = "this machine's theme"), `variant` |
| `theme_reply` | server → client | `name`, `variant`, `tokens: map<token, color>` |

`tokens` is a flat map whose **every value is a literal color** in the spelling
the theme files already use — `#rrggbb`, a palette index (`0`–`255`), or `none`:

```console
$ arreo theme export pushed
theme: pushed (dark) · 58 tokens
                   TOKEN  COLOR
                  accent  #d9c6a5
              background  #121110
                 primary  #123456
                question  #00ff00
                    text  #f0f0f0
                    …            (55 more)
```

`primary` is `"pushedInk"` in the file — a `defs` reference — and `#123456` on
the wire, because the machine resolved it before sending. There is no name in
`tokens` that is not a color.

`arreo theme export --json` prints that shape verbatim, so a script reads exactly
what a client receives:

```console
$ arreo theme export pushed --json | head -c 100   # truncated by head
{"name":"pushed","tokens":{"accent":"#d9c6a5","background":"#121110","backgroundElement":"#0c0b0a","
```

**What is deliberately not on the wire**, and why:

- **`defs` and their references.** `schema::resolve(name, raw, variant)` already
  produces the flat table `Theme::new` takes, so that table travels. Sending the
  file would push `defs` resolution, token-name validation (`NUMERIC_TOKENS`,
  unknown-token refusals) and per-variant unwrapping onto every client: a phone
  and a browser would each carry the resolver, and each would be a place the
  resolution could diverge — the opposite of one theme everywhere.
- **Depth.** The tokens are the theme file's own colors, **unquantized**. The
  receiving surface quantizes (`ThemeTokens::to_theme(depth)`, or
  `color_quantize` across the FFI boundary); see §3.
- **A path.** Nothing in the reply names a file, so a surface with no filesystem
  is a first-class client.

## 3. The one-document rule

One document, and the receiving surface decides how much of it to show:

| Surface | Depth | What it does with `#00ff00` |
| --- | --- | --- |
| iTerm2 / Alacritty / kitty | `truecolor` | emits `38;2;0;255;0` |
| Windows Terminal, xterm-256 | `256` | picks the nearest cube index, `38;5;N`, N ≥ 16 |
| legacy Terminal.app / conhost | `16` | picks from the terminal's own palette, `38;5;N`, N ≤ 15 |
| `NO_COLOR`, `TERM=dumb` | `none` | emits no color sequence at all |

The same reply is therefore correct on all four — which is the acceptance
criterion "depth fallback survives the wire", asserted at the truecolor and
16-colour extremes by `cargo xtask e2e --slice theme` on a real pty, and by
`one_received_document_renders_at_both_depths` in the engine's own tests.

A server that quantized before sending would make the truecolor surface show the
16-colour approximation: one theme with two looks.

## 4. Asking a machine for its theme

```console
$ arreo theme export              # empty name: "what is this machine's theme?"
theme: arreo (dark) · 58 tokens
…
$ arreo theme export --variant light | head -n 3
theme: arreo (light) · 58 tokens
                   TOKEN  COLOR
                  accent  #6c6352
$ arreo theme export solarized
theme: theme "solarized" not found (built-ins: arreo, tokyonight, catppuccin, gruvbox, system)
$ echo $?
1
```

Three answers, and none of them is a page of nothing:

- **A name this machine has** → the resolved tokens, whatever variant was asked
  for.
- **No name at all** → the machine's own theme. A machine with no theme
  configured answers with the built-in default (`Catalog::builtin()`): the look
  that ships in the binary is a *working answer*, not a failure.
- **A name it does not have** → a typed refusal naming the name, with the
  loader's own sentence (which also lists the built-ins). A client that asked for
  `solarized` and silently received `arreo` would have no way to learn its theme
  is not the one it asked for.

`--machine <name>` works like every other read verb, so a phone (or a script on
your laptop) can fetch the palette of a daemon on another machine through the
same verb.

## 5. Clients today, and what is stable

- **The TUI** asks the machine for its theme on the connection it already holds
  (the sidebar's poller, so the theme costs no second session and a remote
  machine's audit trail gains no extra connect/disconnect pair from it) and
  applies the answer **locally at its own depth**. A daemon that is not up yet, or
  one too old to know the verb, changes nothing: the local catalog is the
  fallback, exactly as before T-0116.
- **`arreo theme export`** is the round trip made inspectable without a phone: it
  prints what the verb returned, and its `--json` is the wire shape.
- **The phone** builds its theme from the received tokens across the FFI
  boundary (`theme_from_tokens`, `ThemeHandle::with_depth`, `color_quantize`) —
  T-0104's surface, unchanged by this verb.
- **A browser** (Phase 4) consumes the same `token → color` map.

**N−1** (ADR 0017): the ops are new variants, so a peer that has never heard of
them refuses `theme` typed and keeps the session open — the client falls back
rather than losing the connection. The request's `name` and `variant` are both
`#[serde(default)]`, so a frame from a build before either field still decodes.
The frozen corpora and both directions are exercised by
`cargo xtask e2e --slice compat`.

**If the resolved map ever turns out to be insufficient** for some surface, the
finding is recorded in `tasks/T-0116-the-server-can-push-a-theme-so-one-theme-reaches-every-surface.md`
rather than a second format being added beside it.
