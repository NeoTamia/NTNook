# Nook

Nook est une CLI Rust pour Linux destinée à exposer des services locaux sous des domaines stables `*.localhost`.

Le projet contient actuellement la fondation modulaire ; le contrat complet des commandes sera implémenté dans les tickets suivants.

## Validation

```sh
cargo fmt --check
cargo check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```
