# Model prices

[← wiki](README.md)

Published API list prices, US$ per million tokens, collected **2026-08-29**
from each vendor's own pricing page.

**This is a reference, not the source of truth for anything running.**
`agent-usage` prices tokens from `LIST_RATES` in
[`widgets/src/widgets/agent-usage/main.rs`](../widgets/src/widgets/agent-usage/main.rs), which
carries its own `LIST_RATES_AS_OF` date and is what the pane actually
multiplies by. This page records what was published, so the two can be
compared and the code updated deliberately rather than from memory.

As of 29 Aug 2026 the two agree — see
[what this collection changed](#what-this-collection-changed) for what moved.

## The two rules that shape every table here

**An absent price is not a zero.** A missing key means the vendor does not
charge for that kind, or does not publish it. A zero means they publish it as
free. Conflating them turns "not billed" into "billed at nothing" — the same
number, different facts.

**A model with no published price is named, never inferred.** Rates are
matched by longest prefix, so a model left out silently inherits its family's
rate: a number nobody published, shown as a fact. The
[unpriced list](#models-with-no-published-price) exists to stop that.

## Anthropic

<https://docs.claude.com/en/docs/about-claude/pricing>

Cache writes come in two durations at different prices and the transcripts
record which was taken, so both are carried.

| model | input | output | cache_read | cache_write | cache_write_1h |
|---|--:|--:|--:|--:|--:|
| `claude-fable-5` | 10 | 50 | 1 | 12.50 | 20 |
| `claude-mythos-5` | 10 | 50 | 1 | 12.50 | 20 |
| `claude-opus-5` | 5 | 25 | 0.50 | 6.25 | 10 |
| `claude-opus-4-8` | 5 | 25 | 0.50 | 6.25 | 10 |
| `claude-opus-4-7` | 5 | 25 | 0.50 | 6.25 | 10 |
| `claude-opus-4-6` | 5 | 25 | 0.50 | 6.25 | 10 |
| `claude-opus-4-5` | 5 | 25 | 0.50 | 6.25 | 10 |
| `claude-opus-4-1` | 15 | 75 | 1.50 | 18.75 | 30 |
| `claude-opus-4` | 15 | 75 | 1.50 | 18.75 | 30 |
| `claude-sonnet-5` | 2 | 10 | 0.20 | 2.50 | 4 |
| `claude-sonnet-4-6` | 3 | 15 | 0.30 | 3.75 | 6 |
| `claude-sonnet-4-5` | 3 | 15 | 0.30 | 3.75 | 6 |
| `claude-sonnet-4` | 3 | 15 | 0.30 | 3.75 | 6 |
| `claude-haiku-4-5` | 1 | 5 | 0.10 | 1.25 | 2 |
| `claude-3-5-haiku` | 0.80 | 4 | 0.08 | 1 | 1.60 |

- **Thinking tokens bill as output.** They are not a sixth kind.
- `claude-sonnet-5`'s introductory $2/$10 is now standard; the rise to $3/$15
  scheduled for 1 Sep 2026 was cancelled.
- Fast mode, where offered, is a different rate — `claude-opus-5` and
  `claude-opus-4-8` are $10/$50 in fast mode, first-party only. Not modelled.
- A US `inference_geo` carries a 1.1x multiplier on 4.6 and later.
- The tokenizer changed: 4.7+, Mythos and Fable produce roughly 30% more
  tokens for the same text than Sonnet 4.6 and earlier, so a per-token
  comparison across that boundary understates the newer models.

## OpenAI

<https://developers.openai.com/api/docs/pricing>

| model | input | output | cache_read | cache_write |
|---|--:|--:|--:|--:|
| `gpt-5.6-sol` | 4 | 20 | 0.40 | 5 |
| `gpt-5.6-terra` | 2 | 12 | 0.20 | 2.50 |
| `gpt-5.6-luna` | 0.20 | 1.20 | 0.02 | 0.25 |
| `gpt-5.6-cyber` | 12.50 | 75 | 1.25 | 15.625 |
| `gpt-5.5` | 5 | 30 | 0.50 | — |
| `gpt-5.5-pro` | 30 | 180 | — | — |
| `gpt-5.5-cyber` | 12.50 | 75 | 1.25 | — |
| `gpt-5.4` | 2.50 | 15 | 0.25 | — |
| `gpt-5.4-mini` | 0.75 | 4.50 | 0.075 | — |
| `gpt-5.4-nano` | 0.20 | 1.25 | 0.02 | — |
| `gpt-5.4-pro` | 30 | 180 | — | — |
| `gpt-5.3-codex` | 1.75 | 14 | 0.175 | — |
| `gpt-5.2` | 1.75 | 14 | 0.175 | — |
| `gpt-5.2-pro` | 21 | 168 | — | — |
| `gpt-5.2-codex` | 1.75 | 14 | 0.175 | — |
| `gpt-5.1` | 1.25 | 10 | 0.125 | — |
| `gpt-5.1-codex` | 1.25 | 10 | 0.125 | — |
| `gpt-5.1-codex-max` | 1.25 | 10 | 0.125 | — |
| `gpt-5.1-codex-mini` | 0.25 | 2 | 0.025 | — |
| `gpt-5` | 1.25 | 10 | 0.125 | — |
| `gpt-5-codex` | 1.25 | 10 | 0.125 | — |
| `gpt-5-mini` | 0.25 | 2 | 0.025 | — |
| `gpt-5-nano` | 0.05 | 0.40 | 0.005 | — |
| `gpt-5-pro` | 15 | 120 | — | — |
| `codex-mini-latest` | 1.50 | 6 | 0.375 | — |
| `gpt-4.1` | 2 | 8 | 0.50 | — |
| `gpt-4.1-mini` | 0.40 | 1.60 | 0.10 | — |
| `gpt-4.1-nano` | 0.10 | 0.40 | 0.025 | — |
| `gpt-4o` | 2.50 | 10 | 1.25 | — |
| `gpt-4o-mini` | 0.15 | 0.60 | 0.075 | — |
| `o1` | 15 | 60 | 7.50 | — |
| `o1-pro` | 150 | 600 | — | — |
| `o3` | 2 | 8 | 0.50 | — |
| `o3-pro` | 20 | 80 | — | — |
| `o3-mini` | 1.10 | 4.40 | 0.55 | — |
| `o4-mini` | 1.10 | 4.40 | 0.275 | — |

- **The 5.6 family now has a `cache_write` price.** Earlier families did not,
  which is why the code comment says OpenAI does not charge for cache writes.
  That is no longer true for 5.6.
- **Long context is a different rate, not a surcharge on part of the
  request.** Above the threshold — 272k for most, 200k for the 5.6 family —
  the *whole* request bills at roughly double. `agent-usage` carries one rate
  per kind and cannot express this, so long conversations are **understated**.
  `gpt-5.6-sol` above the line: 8 / 30 / 0.80 / 10.
- `gpt-5.6` and `gpt-daybreak-blue-latest` alias `gpt-5.6-sol`;
  `gpt-daybreak-red-latest` aliases `gpt-5.6-cyber`.
- Reasoning tokens bill as output. Regional data-residency endpoints add 10%
  for models released on or after 5 Mar 2026. Batch, Flex, Fast and
  fine-tuned inference are separate tables, out of scope.

## xAI

<https://docs.x.ai/docs/models>

| model | input | output | cache_read |
|---|--:|--:|--:|
| `grok-4.6` | 2 | 6 | 0.50 |
| `grok-4.5` | 2 | 6 | 0.30 |
| `grok-4.3` | 1.25 | 2.50 | 0.20 |
| `grok-4.20-0309-reasoning` | 1.25 | 2.50 | 0.20 |
| `grok-4.20-0309-non-reasoning` | 1.25 | 2.50 | 0.20 |
| `grok-4.20-multi-agent-0309` | 1.25 | 2.50 | 0.20 |
| `grok-build-0.1` | 1 | 2 | 0.20 |

- **The long-context rule is harsher here.** If the prompt *reaches* 200k
  tokens the whole request bills at roughly double.
- `grok-build-0.1` is where `grok-code-fast-1` and `grok-code-fast` are now
  priced; the rates appear only under the new name.
- No cache-write price is published for any of them.

## Google

<https://ai.google.dev/gemini-api/docs/pricing>

| model | input | output | cache_read |
|---|--:|--:|--:|
| `gemini-3.7-flash` | 0.75 | 3.75 | 0.075 |
| `gemini-3.6-flash` | 0.75 | 3.75 | 0.075 |
| `gemini-3.5-flash` | 1.50 | 9 | 0.15 |
| `gemini-3.5-flash-lite` | 0.30 | 2.50 | 0.03 |
| `gemini-3.1-pro-preview` | 2 | 12 | 0.20 |
| `gemini-3.1-flash-lite` | 0.25 | 1.50 | 0.025 |
| `gemini-3-flash-preview` | 0.50 | 3 | 0.05 |
| `gemini-2.5-pro` | 1.25 | 10 | 0.125 |
| `gemini-2.5-flash` | 0.30 | 2.50 | 0.03 |
| `gemini-2.5-flash-lite` | 0.10 | 0.40 | 0.01 |

- **3.7 and 3.6 Flash are introductory prices through 31 Dec 2026**, doubling
  on 1 Jan 2027 to 1.50 / 7.50 / 0.15. This page goes stale on that date.
- **Context caching is billed by storage — $/1M tokens per *hour*** — which is
  not a per-request `cache_write` and is deliberately not carried. Pricing it
  as one would be inventing a number.
- Audio input costs more than text on several models; only text is listed.
- Prompts over 200k roughly double on `gemini-2.5-pro` and `gemini-3.1-pro`.

## GitHub Copilot

**Deliberately not carried.** Copilot publishes per-token rates under
marketing names — `Claude Sonnet 5`, `GPT-5.6 Sol` — but the local logs record
the **API id** (`claude-sonnet-5`). A table keyed on marketing names would
never match, and carrying both invites a wrong prefix hit.

Its rates also differ from first-party in places: Copilot lists `cache_read`
$0.50 for Grok 4.5 where xAI publishes $0.30. If Copilot spend is ever priced
separately it needs its own table, not these rows.

## Models with no published price

Named so prefix matching cannot hand them a family rate.

| model | why |
|---|---|
| `codex-auto-review` | Not on the API. **The most common entry in local Codex logs.** |
| `gpt-5.4-cyber` | Row published as dashes. |
| `gpt-oss-120b`, `gpt-oss-20b` | Open weights, download rather than API. |
| `grok-4`, `grok-3`, `grok-2` | Off the current pricing table. |
| `grok-4-0709` | Retired 15 May 2026, redirects to `grok-4.3`. |
| `claude-mythos-preview` | Deprecated; use `claude-mythos-5`. |
| `gemini-2.0-flash`, `gemini-2.0-flash-lite`, `gemini-3-pro-preview` | Shut down. |
| `gemma-4` | Free tier only; paid rates listed as unavailable. |
| embeddings, moderation, TTS, image, audio, video | Priced per item or per second, not per text token. |

## What this collection changed

`LIST_RATES` was carrying prices dated `Aug 2026`. Reconciling it against the
above moved three rows, fixed one key that had never worked, and roughly
doubled the table. Applied on 29 Aug 2026.

**Prices that had moved:**

| model | was | now |
|---|---|---|
| `gpt-5.6-sol` | 5 / 30 / 0.50 | **4 / 20 / 0.40**, plus `cache_write` 5 |
| `gpt-5.6-terra` | 2 / 12 / 0.20 | unchanged, plus `cache_write` 2.50 |
| `gpt-5.6-luna` | 0.20 / 1.20 / 0.02 | unchanged, plus `cache_write` 0.25 |

`gpt-5.6-sol` is the most-used priced model in the local Codex logs, so its
cost was **overstated by half on output** for as long as the price sat stale.

**A key that never matched anything.** The table shipped `claude-haiku-3-5`.
Anthropic's id is `claude-3-5-haiku-20241022` — version before name — so the
entry priced nothing for its whole life, and the result looked exactly like a
model nobody had used. `every_model_string_an_agent_writes_down_finds_a_price`
now asserts real logged ids rather than the names we assume, because that is
the only shape of test that could have caught it.

**Added:** the `gpt-5.x-codex` family, `codex-mini-latest`, `gpt-5.6-cyber`,
`gpt-5.5-cyber`, the base `claude-opus-4` and `claude-sonnet-4`, all seven
Grok models and all ten Gemini ones — which is why those agents' spend used to
read as unpriced.

**Still not expressible.** Long context is a different rate rather than a
surcharge, so one rate per kind understates a conversation past the threshold.
That is a table-shape problem, not a data problem.

## Which names actually appear

Rates match by longest prefix against whatever string the agent wrote down, so
only names that appear are worth carrying. From local logs:

| agent | records |
|---|---|
| `claude` | `claude-opus-5`, `claude-opus-4-8`, `claude-fable-5`, `claude-sonnet-5`, `claude-haiku-4-5-20251001` |
| `codex` | `codex-auto-review`, `gpt-5.6-sol`, `gpt-5.6-luna` |
| `copilot` | `claude-sonnet-5` — the API id, not the marketing name |

Two things follow. **A dated id prefix-matches its alias**, so
`claude-haiku-4-5-20251001` is priced by the `claude-haiku-4-5` key and dated
snapshots need no rows of their own. And the Claude logs contain bare `fable`
and `sonnet` strings that match nothing — a small number of unpriced records.

## Refreshing this

Ask an agent with web access for the vendors' own pricing pages, requiring a
citable source per model, omitted keys rather than zeros, and an explicit
unpriced list. The two rules at the top are the load-bearing instructions.

Then update `LIST_RATES`, bump `LIST_RATES_AS_OF`, and **diff the prices that
moved before committing** — a rate that changes quietly is a cost figure that
was wrong for however long it sat there.
