# devin-tokenviz

A pure-Rust CLI that visualizes [Devin CLI](https://devin.ai) token usage and
prices it out. It scans your local Devin CLI session transcripts and prints a
summary: token distribution by model, a cost estimate per model, and a usage
timeline. One-shot output — no fullscreen TUI.

## Install

```sh
cargo install --path .        # from this repo
# or
cargo install --git https://github.com/lexoliu/devin-tokenviz
```

## Usage

```sh
devin-tokenviz            # all recorded history (default: `all`)
devin-tokenviz day        # last 24 hours, hourly timeline  (alias: 24h)
devin-tokenviz week       # last 7 days, daily timeline
devin-tokenviz month      # last 30 days, daily timeline
```

Global flags: `--data-dir DIR` (default `~/.local/share/devin/cli/transcripts`),
`--db FILE` (default `<data-dir>/../sessions.db`), `--transcripts-only`,
`--pricing FILE` (extra pricing rules).

Example output:

```text
devin-tokenviz · last 7 days · Sep 06 16:55 → Sep 12 03:06
3777 sessions · 23108 calls · 2.87B tokens
coverage: 74 transcripts · +19667 calls (2.41B tok) recovered from
          sessions.db — resumed chains & subagent runs
input 149M · cached 2.71B · output 10.7M
list (equiv.) $1,919.07   actual $883.76

── by model ─────────────────────────────────────────────
 MODEL        TOTAL  SHARE  PRICED AS            LIST    ACTUAL  DIST
 SWE-2        1.79B  62.6%  kimi-k3 *          $974.04     $0.00  ████
 GPT-6 Astra   492M  17.2%  list               $733.43   $733.43  ███
 GPT-5.6 Sol   328M  11.4%  list               $150.34   $150.34  ███
 ...
 * free in Devin CLI — struck list price, actual $0.00

── timeline ─────────────────────────────────────────────
 Sep 10  ███████████████████████████  652M  $294.05  $0.00
 ...
```

Colors: cyan = input, gray = cached, yellow = output. Free models show their
equivalent list price struck through with a green `$0.00`. ANSI styling is
disabled when piping or when `NO_COLOR` is set.

## What it reads

Two local sources are merged:

1. **Transcripts** — `~/.local/share/devin/cli/transcripts/*.json`. Each step
   records `prompt_tokens`, `completion_tokens`, and `cached_tokens` per model.
   `cached` is a subset of `prompt`, so uncached input is `prompt − cached`.

2. **`sessions.db`** — `~/.local/share/devin/cli/sessions.db`, the CLI's local
   session store. Transcripts only serialize a session's *current* chain:
   usage from earlier chains after a resume/compact/fork, and from **subagent
   sessions** (which never get a transcript), is not written there. The
   `message_nodes` table keeps every inference call, and each node's
   `metadata.num_tokens_preceding` equals that call's exact `prompt_tokens`
   (verified against the CLI's own `response_dimensions` stats — they agree
   to the token).

   Dedup: a transcript step and a db call are the same call when they share a
   session and an identical prompt-token count (multiset match — retries with
   an identical context still count as extra calls). Db-only calls contribute
   their exact prompt tokens; their cached share and output are estimated from
   the session's transcript ratios (global ratios as fallback), which is why
   the db-derived `cached`/`output` splits are approximate while input totals
   are exact.

   On a real install this roughly **6x'd** the visible token total — most of
   the difference was subagent runs and resumed sessions.

   `message_nodes` is append-only, so matched rows are cached in
   `~/.cache/devin-tokenviz/` and each run scans only the new `row_id` tail —
   repeat runs take ~0.1s after the first full scan (~3s on a multi-GB db).
   Delete the directory to force a rescan.

   Disable with `--transcripts-only`.

## Pricing

Costs are estimated at **public API list prices** (USD per 1M tokens), built in
as of Sep 2026:

| Model | Priced as | In | Cached | Out |
|---|---|---|---|---|
| SWE-1.7 | kimi-k2.7-code | $0.95 | $0.19 | $4.00 |
| SWE-2 | kimi-k3 | $3.00 | $0.30 | $15.00 |
| Adaptive | kimi-k3 (est.) | $3.00 | $0.30 | $15.00 |
| GPT-5.6 Sol / Terra / Luna | list | $4 / $2 / $0.20 | $0.40 / $0.20 / $0.02 | $20 / $12 / $1.20 |
| GPT-6 Astra | list | $10 | $1.00 | $50 |
| Claude Fable / Opus / Sonnet / Haiku | list | $10 / $5 / $2–3 / $1 | … | $50 / $25 / $10–15 / $5 |
| Gemini 3 Pro / Flash | list | $2 / $1.50 | … | $12 / $7.50 |
| GLM-5 (free in Devin) | glm-5 | $1.40 | $0.26 | $4.40 |
| Kimi K3 / K2.7 | list | $3.00 / $0.95 | $0.30 / $0.19 | $15 / $4.00 |

**Free models** (SWE-1.7, SWE-2, Adaptive, Fusion, GLM, Penguin — Cognition's
own models plus open models Devin CLI doesn't bill) are still priced at the
public model they're equivalent to: the list price is shown struck through and
the actual charge is a green `$0.00`. `Adaptive` is a router, so its rate is an
estimate; `Penguin` has no public equivalent and shows `n/a`.

Models with no matching rule show `?` and are excluded from cost totals.

### Custom pricing

Drop rules in `~/.config/devin-tokenviz.toml` or pass `--pricing file.toml`.
Entries take precedence over built-ins; `pattern` is a lowercase substring match
on the normalized model name:

```toml
[[rule]]
pattern = "swe-2"
label = "SWE-2"
free = true
billed_as = "kimi-k3"
input = 3.00    # $/1M uncached input
cached = 0.30   # $/1M cached input (defaults to `input`)
output = 15.00  # $/1M output
```

Set `free = false` (or omit it) for paid models; omit `input` to leave a model
unpriced.

## Notes

- Costs are estimates: cache-write premiums, long-context repricing, and
  batch/fast tiers are not modeled. For db-recovered calls, the cached/output
  split is estimated (input totals are exact).
- 100% Rust: clap, serde_json, chrono, toml, terminal_size, rusqlite
  (bundled SQLite). No TUI framework — output is plain text with optional
  ANSI styling.
