# LiteLLM Model Pricing JSON Format (`pricing import`)

- **Upstream file:** `model_prices_and_context_window.json` (BerriAI LiteLLM)
- **Checked date:** `2026-10-31`
- **Usage in Kakeibo:** Read-only offline import via `kakeibo pricing import <file>` (`PLAN.md` §0 #4, §0 #6, §6.2, §10 T09). Kakeibo never fetches this file over the network; users download or maintain a local JSON file and import it into Kakeibo's local `pricing.toml`.

## Sources

- **[S1]** BerriAI/litellm — `model_prices_and_context_window.json` canonical schema & `"sample_spec"` header: <https://github.com/BerriAI/litellm/blob/main/model_prices_and_context_window.json> (`checked: 2026-10-31`).
- **[S2]** LiteLLM Documentation — Custom Pricing & Prompt Caching Cost Keys (`cache_creation_input_token_cost`, `cache_creation_input_token_cost_above_1hr`, `cache_read_input_token_cost`, `output_cost_per_reasoning_token`): <https://docs.litellm.ai/docs/completion/token_usage> (`checked: 2026-10-31`).

## Top-Level Structure

The JSON root is an object mapping model identifiers (either bare model names like `"claude-sonnet-4-5"` or provider-prefixed keys like `"anthropic/claude-opus-4-5"`) to descriptor objects [S1]:

- `"sample_spec"` is LiteLLM's inline schema documentation object at the top of `model_prices_and_context_window.json` and **must be skipped** during import [S1].
- Entries missing either `input_cost_per_token` or `output_cost_per_token` (e.g. unpriced aliases, image-only models, or incomplete stubs) are counted as skipped rather than imported with invented `$0` prices (`PLAN.md` §0 #7, §6.2).

## Field Reference (Every Used Field Cites a Source)

LiteLLM stores token rates in **USD per single token** (`f64`). During `pricing import`, Kakeibo converts each finite non-negative per-token rate to **USD per 1,000,000 tokens** (`rate_unit = "per_1m_tokens"`) by multiplying by `1_000_000.0`.

| Field Path | JSON Type | Kakeibo `pricing.toml` Target | Semantics | Source |
|---|---|---|---|---|
| `<model_key>.input_cost_per_token` | `number` | `input` | Standard uncached fresh input token rate (USD/token). Required for an entry to be imported as priced. | [S1], [S2] |
| `<model_key>.cache_creation_input_token_cost` | `number` | `cache_write_5m` | Default (5-minute ephemeral TTL) prompt-cache creation/write token rate (USD/token). | [S1], [S2] |
| `<model_key>.cache_creation_input_token_cost_above_1hr` | `number` | `cache_write_1h` | Extended 1-hour ephemeral TTL prompt-cache creation/write token rate (USD/token). | [S1], [S2] |
| `<model_key>.cache_read_input_token_cost` | `number` | `cache_read` | Discounted prompt-cache hit/read token rate (USD/token). | [S1], [S2] |
| `<model_key>.output_cost_per_token` | `number` | `output` | Standard completion/output token rate (USD/token). Required for an entry to be imported as priced. | [S1], [S2] |
| `<model_key>.output_cost_per_reasoning_token` | `number` | `reasoning` | Optional distinct rate for reasoning/thinking tokens (USD/token). When present, reasoning tokens inside `output` are billed at this rate without double-counting `output`. | [S1], [S2] |
| `<model_key>.litellm_provider` | `string` | *(metadata, ignored)* | Provider slug recorded by LiteLLM (`"anthropic"`, `"openai"`, `"gemini"`, etc.). | [S1] |
| `<model_key>.mode` | `string` | *(metadata, ignored)* | Endpoint mode (`"chat"`, `"responses"`, `"completion"`, `"embedding"`, etc.). | [S1] |
| `<model_key>.max_input_tokens` | `integer` | *(not imported into `pricing.toml`)* | Context window metadata; Kakeibo keeps verified context windows separately in `data/models.toml` (`PLAN.md` §6.2). | [S1] |
