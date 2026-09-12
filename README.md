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
`--pricing FILE` (extra pricing rules).

Example output:

```text
devin-tokenviz · last 7 days · Sep 06 16:55 → Sep 12 02:30
74 sessions · 3602 steps · 467M tokens
input 15.3M · cached 449M · output 1.86M
list (equiv.) $199.56   actual $79.89

── by model ─────────────────────────────────────────────
 MODEL        TOTAL  SHARE  PRICED AS            LIST    ACTUAL  DIST
 SWE-2         158M  33.8%  kimi-k3 *           $83.18     $0.00  ████
 SWE-1.7       155M  33.2%  kimi-k2.7-code *    $36.25     $0.00  ████
 GPT-5.6 Sol   144M  31.0%  list                $66.18    $66.18  ███
 ...
 * free in Devin CLI — struck list price, actual $0.00

── timeline ─────────────────────────────────────────────
 Sep 09  ██████████████████████████████  218M  $82.57  $66.18
 ...
```

Colors: cyan = input, gray = cached, yellow = output. Free models show their
equivalent list price struck through with a green `$0.00`. ANSI styling is
disabled when piping or when `NO_COLOR` is set.

## What it reads

Session transcripts at `~/.local/share/devin/cli/transcripts/*.json`. Each step
records `prompt_tokens`, `completion_tokens`, and `cached_tokens` per model.
`cached` is a subset of `prompt`, so uncached input is `prompt − cached`.

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
which Devin CLI doesn't bill) are still priced at the public model they're
equivalent to: the list price is shown struck through and the actual charge is
a green `$0.00`. `Adaptive` is a router, so its rate is an estimate.

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
- 100% Rust: clap, serde_json, chrono, toml, terminal_size. No TUI framework —
  output is plain text with optional ANSI styling.
