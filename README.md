# devin-tokenviz

A pure-Rust TUI that visualizes [Devin CLI](https://devin.ai) token usage and
prices it out. It scans your local Devin CLI session transcripts and shows a
token-distribution breakdown plus a per-model and per-session cost estimate.

## Install

```sh
cargo install --path .        # from this repo
# or
cargo install --git https://github.com/lexoliu/devin-tokenviz
```

## Usage

```sh
devin-tokenviz                 # launch the TUI
devin-tokenviz --print         # plain-text summary (no TUI)
devin-tokenviz --days 7        # only the last 7 days
devin-tokenviz --data-dir DIR  # custom transcripts directory
devin-tokenviz --pricing FILE  # extra pricing rules (TOML)
```

Keys in the TUI: `q`/`Esc` quit · `↑↓`/`jk` select session · `g`/`G` top/bottom ·
`s` cycle session sort (recent → cost → tokens → name) · `r` rescan transcripts.

## What it reads

Session transcripts at `~/.local/share/devin/cli/transcripts/*.json`
(`--data-dir` overrides). Each step records `prompt_tokens`,
`completion_tokens`, and `cached_tokens` per model. `cached` is a subset of
`prompt`, so uncached input is `prompt − cached`.

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
| GLM-5 | list | $1.40 | $0.26 | $4.40 |
| Kimi K3 / K2.7 | list | $3.00 / $0.95 | $0.30 / $0.19 | $15 / $4.00 |

**Free models** (SWE-1.7, SWE-2, Adaptive, Fusion — Cognition's own models,
which Devin CLI doesn't bill) are still priced at the equivalent public model
they're based on: the list price is shown ~~struck through~~ and the actual
charge as green `$0.00`. `Adaptive` is a router, so its rate is an estimate.

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
  batch/fast tiers are not modeled.
- 100% Rust: ratatui + crossterm, serde_json, clap, chrono, toml.
