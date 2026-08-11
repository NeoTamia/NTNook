# Nook 0.1.0 — notes de release MVP

Cette version fournit un unique binaire Linux `nook`. L’artefact CI `nook-linux-x86_64` contient le binaire, sa somme SHA-256, le README et ces notes.

## Fonctionnalités livrées

- lancement supervisé d’une application locale sous un domaine `*.localhost` en HTTPS ou HTTP ;
- aliases persistants vers un port ou une URL HTTP(S) ;
- mutation atomique et propriétaire des routes d’une instance Caddy existante ;
- commandes `list`, `status`, `stop`, `prune` et récupération après interruption ;
- vérification TLS des upstreams HTTPS et diagnostic de la CA locale Caddy.

## Plateforme et dépendances

- Linux x86-64 ;
- Caddy `2.11.x` installé et démarré séparément, avec Admin API accessible ;
- serveurs Caddy non ambigus sur `:443` et, pour `--no-tls`, sur `:80`.

La compilation reproductible utilise Rust `1.97.1` et `Cargo.lock`. OpenSSL, Python 3, curl avec HTTP/2, util-linux et iproute2 sont uniquement requis par les tests d’intégration, pas par le binaire.

## Hors périmètre

Le binaire ne fournit aucun daemon, IPC, socket local, serveur embarqué, shell implicite, modification de `/etc/hosts`, installation ou démarrage de Caddy, installation automatique de CA, intégration Docker, LAN/mDNS, multi-service, support Windows/macOS, Tailscale Serve/Funnel ou exposition publique.

Avant publication, l’artefact doit provenir d’une exécution CI verte et sa somme doit être vérifiée avec `sha256sum --check nook.sha256` depuis son répertoire.
