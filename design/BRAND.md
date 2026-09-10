# Arreo — Brand & UI Design System (v1)

> References studied: Herdr landing (near-black, huge grotesque type, violet accent, terminal
> mock below the fold) and the Herdr TUI/app (sidebar grouped by state, colored dots, violet
> highlight). Orca mobile (dark sidebar, worktree cards, purple accents).
> **Our job: same confidence, different identity.** No violet, no coral. Cyan × sand on warm
> charcoal, state colors as the loudest element on screen, denser information layout.

---

## 1. Identity

| Token | Value | Notes |
| --- | --- | --- |
| Name in copy | **Arreo** — lowercase in prose, "arreo" in commands | the CLI is the brand |
| Tagline | *"Run 30 agents on your server. Herd them from your phone."* | |
| Logo | a braid (`trenza`) knot mark: three interlocking strands = agents, machines, humans; doubles as a herding knot. Monochrome, works at 16 px. | no cute animal mascots — Herdr owns the sheep |
| Voice | plain, technical, warm. Short sentences. Numbers over adjectives. Never shouty; the product is the flex. | |
| Tone on landing | "we built this for ourselves, it works, here's the proof" — tables, budgets, real numbers | Herdr style kept, content denser |

## 2. Palette (base — becomes the default `arreo` theme)

Warm charcoal base (not blue-black like Herdr's violet cast), cream text, **cyan primary**,
**sand** as the editorial second accent. State colors are reserved and never used decoratively.

| Token | Hex | Use |
| --- | --- | --- |
| `bg` | `#121110` | page/app background (warm charcoal, faint brown undertone) |
| `bg-elevated` | `#1A1816` | cards, panels, modals |
| `bg-inset` | `#0C0B0A` | terminal wells, code blocks |
| `border` | `#2A2724` | hairlines |
| `border-active` | `#3E3A35` | hover/focus borders |
| `text` | `#EDE7DC` | body text (cream — softens the contrast vs pure white) |
| `text-muted` | `#9A938A` | secondary |
| **`primary`** | **`#6FD3E8`** | cyan — actions, links, active states, logo |
| `primary-strong` | `#A8E4F2` | hero accent words, selection |
| **`accent-sand`** | **`#D9C6A5`** | editorial: kickers, section labels, italic asides, lasso motifs |
| **`working`** | **`#6FD3E8`** (cyan) | agent working — same family as primary |
| **`question`** | **`#E8B45A`** (amber) | blocked on a question — pulsing dot; THE signal color |
| **`blocked`** | **`#FF9E64`** (orange) | blocked, needs attention, no question detected |
| **`done`** | **`#8FD19E`** (sage green) | done / success |
| **`error`** | **`#E5636F`** | failures |
| `idle` | `#6E6A64` | idle/unknown |

Rules:

- **State colors are sacred.** A cyan button is fine; a cyan *sidebar* is not — cyan always
  means "working". Amber/orange never appears in decoration. This is what makes the status
  board readable across the room.
- Dark-first; light variants derived (cream becomes paper `#F5F1E8`, bg `#FAF8F4`, states
  shift one step darker for contrast).
- Terminal aesthetics without cosplay: monospace for *data* (commands, numbers, states),
  humanist sans for prose. No scanlines, no CRT glow.

## 3. Typography

| Role | Face | Notes |
| --- | --- | --- |
| Display / hero | **Archivo Expanded** (or Variable Archivo at 125% width) | Herdr's neighbor — heavy, wide, unapologetic. Accent word in cyan. |
| UI / body | **Inter** | prose, nav, cards |
| Data / mono | **JetBrains Mono** | commands, stats, states, terminal |

Scale: hero 72–96 px (@125% width, -2% tracking), section titles 40 px, kicker 12 px
mono uppercase letterspaced 0.2em with a 24 px sand dash before it (Herdr does this — keep,
it reads), body 17 px/1.6.

## 4. Landing page (the differences vs Herdr, concretely)

Herdr's strengths we keep: giant type hero, install box with copy button, stats strip,
section kickers, live TUI mock. Our changes:

1. **The hero shows the product, not the logo.** Behind/under the headline sits the
   *status board* — a real Arreo view: 30 agent chips grouped by state, one amber
   `question` card with quick-reply input. The abstract braid watermark is background
   texture only (Herdr's sheep silhouette spot).
2. **Warm charcoal, not violet-black; cyan accent, sand editorial.** Instantly distinguishable
   at 10 m.
3. **"The moment" section right after the fold:** a phone push card ("workbox · pi-agent is
   asking: May I run `sudo apt install`? [Allow] [Deny]") beside the same moment in TUI,
   phone, and web — one state, three surfaces, one screenshot row.
4. **Numbers row:** same 4-slot pattern but *resource-truth flavored*: agents running,
   harness RAM freed, questions answered from phone, update handoffs with zero agent loss.
5. **Sections:** The moment → Machines mesh (VPS+RPi diagram) → Config sync (edit once) →
   Zero-cut updates → Themes (interactive: theme picker swaps the page palette live) →
   Plugins → Comparison table → Pricing → Install CTA.
6. **Theme picker on the landing itself** — the page ships with 4 built-in palettes
   (arreo/tokyonight/catppuccin/system) switchable live: the theming feature *is* the demo.
7. Footer: braid mark, docs, security/threat-model, status, source.

Layout system: 12-col grid, 1152 px content, generous 120 px section spacing, hairline
borders (1 px `border`) as separators — density with air. States strip (chips) recurs as a
section header device: each section opens with its state color chip.

## 5. Web dashboard (app.arreo.dev — distinct from the landing, deliberately)

The dashboard is a **control plane**, not a website: denser, quieter, no marketing chrome.

```text
┌──────────────────────────────────────────────────────────────────────┐
│ ⌘ arreo   search (⌘K) …            workbox ▾   rpi5 ▾    ⚙  theme ▾  │
├────────────┬─────────────────────────────────────────┬───────────────┤
│ MACHINES   │  AGENT GRID                             │ ATTENTION     │
│ ● workbox  │  ┌──────────┐ ┌──────────┐ ┌─────────┐  │               │
│   12 · 2 ask│ │ pi-auth  │ │ codex-ci │ │ pi-docs │  │ 2 questions   │
│ ● rpi5     │  │ ● working│ │ ● question│ │ ● done  │  │ 1 blocked     │
│   8 · 0    │  │ 342 MB   │ │ 512 MB   │ │ 96 MB   │  │               │
│ ○ laptop   │  └──────────┘ └──────────┘ └─────────┘  │ ▸ pi-auth     │
│   last 3h  │  …(virtualized, 30 cards)               │   "sudo apt…" │
│            │                                         │   [Allow][Deny]│
│ QUEUES     │  selected agent → drawer: terminal,     │ ▸ codex-ci    │
│ 0 offline  │  metrics graph, prompt, approvals       │   "run tests?" │
├────────────┴─────────────────────────────────────────┴───────────────┤
│ status bar: relay · version · 30 agents · RAM 118 MB · audit link     │
└──────────────────────────────────────────────────────────────────────┘
```

- **Left rail:** machines (presence dot, agent count, question count in amber), then sync
  queues. Collapses to icons.
- **Center:** agent cards in a responsive grid (not a terminal pane wall — the terminal is
  one click away, not the default). Card = state dot + pulse, name, harness, RAM bar,
  branch/task, last line (1-line ellipsis). Click → detail view (terminal well, metrics
  graph, quick actions).
- **Right rail:** the attention queue — every `question`/`blocked` with quick actions.
  This rail *is* the paid web differentiator: your fleet's inbox.
- **Top:** machine switcher, ⌘K search across agents/machines/config, theme menu (same
  theme JSON as everywhere).
- Density: 13 px UI text, 4/8 px spacing scale, hairlines over shadows.

## 6. Mobile app (iOS/Android — same system, thumb-first)

- **Bottom tabs (4):** `Herd` (overview) · `Attention` (question/blocked queue) ·
  `Approvals` · `Settings`. Attention gets a badge count — the app opens to your pending
  answers by default option.
- **Herd tab:** machine group header (presence, RAM summary) → agent cards: state dot
  (pulsing when question), name, harness chip, 24 px RAM sparkline bar, last line. Pull to
  refresh; live deltas only for the focused card.
- **Agent detail:** header with state + duration, RAM/CPU graph, last 20 lines (virtualized
  mono), quick-reply composer, action row (restart · kill · sleep).
- **Question card (the money screen):** amber border, the question text, context chip
  (`awaiting: sudo apt install`), [Allow] [Deny] [Type answer…], biometric gate on send.
- **Approvals tab:** policy-gated commands, allow/deny with audit note.
- Settings: theme picker (synced from server), devices (revoke), notification rules,
  budgets.
- Palette identical to the web/TUI tokens; dark-first, auto light. No gradients on state
  colors — a question is amber on any platform, anywhere.

## 7. Motion & interaction

- 120–180 ms eases, no bounce; state dots pulse (2 s period) only for `question`.
- Landing: theme swap is a 200 ms cross-fade (it's the product demo); install box copy
  → inline "copied" tick.
- Dashboard: card transitions on state change (dot + border color only — layout is stable,
  states change, layout doesn't jump).
- Reduced-motion respected everywhere (`prefers-reduced-motion`).

## 8. What we deliberately do NOT copy from the references

- Herdr's violet: primary color collides with state colors — violet *is* a state-adjacent
  hue; we need a neutral-action palette that lets amber/orange/green mean something.
- Orca's coral-to-purple gradient blobs: decorative gradients are out; color = meaning.
- Herdr's TUI-as-first-screenshot: ours leads with the status board (the 30-agent glance),
  the terminal view is the detail layer — it positions us as fleet-first, not pane-first.
- Giant watermark mascots as the identity: our mark is the braid; identity comes from the
  state-chip system and sand × cyan pairing.
