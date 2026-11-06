# bmodels

Watch model registries for new and changed AI models, and keep track of what you've already
seen.

bmodels polls provider APIs (OpenAI, Anthropic, Gemini, Mistral, xAI, DeepSeek, Groq, Together,
Cerebras, any OpenAI-compatible server), aggregators (OpenRouter, models.dev, LiteLLM), Hugging
Face orgs, the Ollama library, and RSS/Atom changelogs. It merges the same model across sources
(`claude-sonnet-4-5-20250929` on Anthropic = `anthropic/claude-sonnet-4.5` on OpenRouter), and
remembers what you have acknowledged. You only hear about what is actually new: new models, new
listings, price/context changes, removals.

## Quick start

```sh
cargo install --path packages/bmodels
bmodels sync        # first sync records a silent baseline: nothing is "new" yet
bmodels             # the TUI: inbox, models, sources, history
```

Sources that need an API key are skipped until the key is set (`export OPENAI_API_KEY=...`, or
`api_key_cmd = "op read ..."` in config). The keyless registries work out of the box.

Keep it running in the background with desktop notifications:

```sh
bmodels watch                      # foreground loop honoring each source's interval
bmodels watch --once               # one pass, for cron / launchd
```

## Everyday commands

```sh
bmodels new                        # inbox: what you haven't acknowledged
bmodels ack gpt-6                  # acknowledge a model (by name, key, listing, or #event)
bmodels ack --all --vendor openai  # acknowledge in bulk
bmodels show anthropic/claude-x    # metadata, every listing, change history
bmodels list --since 7d            # models first seen this week
bmodels star / ignore / unsee <model>
bmodels sources                    # health, counts, missing keys
```

Every listing command takes `--json`.

## Configuration

Built-in sources live in an embedded catalog; your file overrides them field by field (layered
with bmux's config composition):

```sh
bmodels config init                # writes a commented starter to ~/.config/bmodels/config.toml
bmodels config check
```

```toml
[sources.ollama]
enabled = false

[sources.local]                    # any OpenAI-compatible server
kind = "OPENAI_COMPATIBLE"
url = "http://localhost:11434/v1"
vendor = "local"

[filters]
exclude = ["*embedding*", "*tts*"]

[notify]
kinds = ["NEW", "CHANGED"]
command = ["sh", "-c", "cat > /tmp/bmodels-events.json"]   # JSON digest on stdin
webhook = "https://example.com/hook"
```

See [`docs/PLAN.md`](docs/PLAN.md) for the architecture.

## Development

Requires sibling checkouts of [bmux](https://github.com/BSteffaniak/bmux) and
[MoosicBox](https://github.com/MoosicBox/MoosicBox) next to this repository, and the nix dev
shell (it provides `libiconv` for linking on macOS):

```sh
nix develop
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## License

MPL-2.0
