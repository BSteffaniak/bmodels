# bmodels

Watch model registries (provider APIs, aggregators like OpenRouter and models.dev, Hugging Face,
Ollama, changelogs) for new and changed AI models, and keep track of what you've already seen.

Status: early scaffold. See [`docs/PLAN.md`](docs/PLAN.md).

## Development

Requires sibling checkouts of [bmux](https://github.com/BSteffaniak/bmux) and
[MoosicBox](https://github.com/MoosicBox/MoosicBox) next to this repository.

```sh
cargo build
cargo clippy --all-targets -- -D warnings
cargo install --path packages/bmodels
```

## License

MPL-2.0
