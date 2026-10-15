# isDown

Projet Rust pour une application native macOS de surveillance de pages de statut depuis la barre de menus.

Le projet contient actuellement uniquement le socle Cargo. L’interface, la surveillance et les notifications ne sont pas encore implémentées.

Le plan fonctionnel, technique et multi-agent est dans [PLAN.md](PLAN.md).

## Développement

Rust stable avec l’édition 2024 ; macOS requis pour la future interface AppKit.

```sh
cargo run
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
```

Aucune dépendance externe n’est ajoutée au socle initial. Les dépendances natives seront sélectionnées et mesurées lors de la première étape du plan.

Le binaire initial ne contient pas encore de tests métier ni de cible bibliothèque. Les doctests seront activés avec les crates du workspace.
