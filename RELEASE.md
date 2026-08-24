# Nook 0.1.0 — notes de release MVP

Cette version fournit un unique binaire Linux `nook`. La GitHub Release contient une archive statique
`nook-x86_64-unknown-linux-musl.tar.xz`, sa somme SHA-256 et l'installateur `nook-installer.sh`.
Les sources prêtes à compiler sont également publiées sur crates.io sous le nom `ntnook`.

## Initialisation crates.io pour les mainteneurs

Le nom `nook` appartenant déjà à un autre projet, le paquet est publié sous le nom `ntnook` tout en
installant la commande `nook`. Trusted Publishing nécessite une première publication manuelle :

```sh
cargo publish --locked
```

Ajoutez ensuite un Trusted Publisher GitHub Actions à la crate `ntnook` avec le dépôt
`NeoTamia/NTNook`, le workflow `publish.yml` et l'environnement `release`. L'environnement GitHub
`release` doit autoriser les tags `v*`. Après une première exécution réussie, crates.io peut être
configuré en mode « Trusted Publishing only ».

## Fonctionnalités livrées

- lancement supervisé d’une application locale sous un domaine `*.localhost` en HTTPS ou HTTP ;
- aliases persistants vers un port ou une URL HTTP(S) ;
- mutation atomique et propriétaire des routes d’une instance Caddy existante ;
- commandes `list`, `status`, `stop`, `prune`, `update` et récupération après interruption ;
- génération de complétions statiques Bash et Zsh avec `nook completions` ;
- vérification TLS des upstreams HTTPS et diagnostic de la CA locale Caddy.

## Plateforme et dépendances

- Linux x86-64 ;
- Caddy `2.11.x` natif ou via l’image Docker officielle, démarré séparément avec Admin API accessible ;
- serveurs Caddy non ambigus sur `:443` et, pour `--no-tls`, sur `:80`.

La compilation reproductible utilise Rust `1.97.1` et `Cargo.lock`. OpenSSL, Python 3, curl avec HTTP/2, util-linux et iproute2 sont uniquement requis par les tests d’intégration, pas par le binaire.

## Hors périmètre

Le binaire ne fournit aucun daemon, IPC, socket local, serveur embarqué, shell implicite, modification de `/etc/hosts`, installation ou démarrage de Caddy, installation automatique de CA, orchestration Docker, LAN/mDNS, multi-service, support Windows/macOS natif, Tailscale Serve/Funnel ou exposition publique.

Le dépôt fournit un Compose Caddy officiel supporté sous Linux et une compatibilité testée avec caddy-docker-proxy. `nook ca export` permet de récupérer la CA publique sans exécutable Caddy sur l’hôte. Voir `docs/DOCKER.md`.

Avant publication, l’artefact doit provenir d’une exécution CI verte. Après téléchargement, vérifiez-le
avec `sha256sum --check nook-x86_64-unknown-linux-musl.tar.xz.sha256` depuis son répertoire.
