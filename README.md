# Nook

Nook est une CLI Linux qui expose des applications locales sous des domaines stables `*.localhost` en configurant une instance Caddy existante.

## Prérequis et installation

- Linux ;
- Rust stable pour compiler Nook ;
- Caddy `2.11.x`, déjà installé, démarré et accessible par son Admin API ;
- des serveurs Caddy non ambigus écoutant sur `:443` pour HTTPS et, si `--no-tls` est utilisé, sur `:80` pour HTTP.

```sh
caddy version
cargo install --path .
nook --help
```

Nook ne démarre ni n’installe Caddy. Il ne lance jamais `sudo`, ne modifie pas `/etc/hosts` et n’installe pas la CA locale. Les noms sous `.localhost` sont résolus nativement vers loopback par les navigateurs et systèmes compatibles.

## Préparer Caddy pour Nook

Nook ajoute ses routes à un serveur Caddy existant : il ne crée pas le listener HTTPS lui-même. Le Caddyfile doit donc produire exactement un serveur écoutant explicitement sur `:443`. Par exemple, ajoutez ce site à votre configuration existante :

```caddyfile
https://localhost {
	tls internal
	respond 404
}
```

Si l’Admin API doit utiliser le socket Unix standard, placez aussi cette directive dans le bloc global existant du Caddyfile :

```caddyfile
{
	admin "unix//run/caddy/admin.socket"
}
```

Validez puis rechargez Caddy :

```sh
sudo caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile
sudo systemctl reload caddy
```

L’utilisateur exécutant Nook doit pouvoir traverser `/run/caddy` et lire/écrire sur le socket. Sur une installation utilisant le groupe `caddy`, ajoutez votre utilisateur à ce groupe une seule fois afin d’utiliser Nook ensuite sans `sudo`, puis déconnectez-vous et ouvrez une nouvelle session :

```sh
sudo usermod -aG caddy "$USER"
```

Après reconnexion, vérifiez que la nouvelle session possède bien le groupe :

```sh
id -nG
```

La sortie doit contenir `caddy`. Ne lancez pas Nook avec `sudo` : ses fichiers de configuration et d’état appartiennent à votre utilisateur, et les processus applicatifs doivent conserver ses permissions normales.

Caddy émet les certificats `*.localhost` avec sa CA locale. Installez explicitement cette CA depuis votre session utilisateur afin que le système et les navigateurs lui fassent confiance :

```sh
caddy trust --address unix//run/caddy/admin.socket
nook --caddy-socket /run/caddy/admin.socket status
```

La seconde commande doit indiquer `trusted` pour `local_ca`. Fermez complètement puis relancez les navigateurs déjà ouverts. Pour une Admin API TCP, utilisez plutôt l’adresse affichée par `nook status`, par exemple `caddy trust --address 127.0.0.1:2019`.

## Lancer une application

```sh
nook run --name api -- bun run dev
nook api run -- bun run dev
nook run --name docs --app-port 5173 --strict-port -- npm run dev
nook run --name legacy --no-tls -- ./server
```

Options de `run` :

- `--name <name>` choisit le domaine ; `.localhost` est ajouté automatiquement ;
- `--no-tls` utilise exclusivement le frontend HTTP ;
- `--app-port <port>` demande un port, avec fallback explicite s’il est occupé ;
- `--strict-port` refuse ce fallback et exige `--app-port` ;
- `--force` transfère un hostname déjà possédé par Nook sans arrêter l’ancien processus ;
- `--config <path>` choisit explicitement le fichier projet ;
- `--readiness-warn-after <seconds>` règle le délai du warning de readiness ;
- les arguments après `--` sont transmis directement, sans shell implicite.

Nook remplace littéralement `{port}` dans chaque argument et injecte `PORT`, `HOST=127.0.0.1` et `NOOK_URL`. Le processus reçoit stdin/stdout/stderr du terminal et son code de sortie est conservé, même si le cleanup Caddy doit être réessayé plus tard.

## Aliases persistants

```sh
nook alias set api 3000
nook alias api https://service.internal:8443 --preserve-host
nook alias set old 8080 --no-tls --force
nook alias list
nook alias remove api
nook alias --remove old
```

Une cible peut être un port entier ou une URL absolue `http://`/`https://`. Credentials, query, fragment et chemins autres que `/` sont refusés. Les certificats HTTPS upstream restent vérifiés ; aucun mode insecure n’est proposé. Un upstream indisponible ne supprime pas l’alias et Caddy répond alors `502`.

Par défaut, l’upstream reçoit son propre `Host`. `--preserve-host` conserve le domaine demandé. `X-Forwarded-Host` conserve toujours le hostname `.localhost`.

## Commandes opérationnelles

```sh
nook list
nook status
nook stop api
nook stop api --force
nook prune
```

- `list` distingue les runs `starting`/`ready` et les aliases persistants ;
- `status` vérifie l’Admin API, les serveurs, les conteneurs Nook, les dérives et la confiance de la CA locale ;
- `stop` envoie SIGTERM au groupe du run actuellement géré ;
- `stop --force` attend au maximum deux secondes puis utilise SIGKILL si le même processus est encore vivant ;
- `prune` nettoie les leases mortes et routes orphelines, rejoue les opérations en attente et restaure les routes manquantes.

Nook ne modifie jamais une route Caddy étrangère, même avec `--force`. Les routes Nook portent un owner UUID ; un ancien processus ne peut donc pas supprimer la route de son remplaçant.

## Configuration projet

Le fichier `nook.toml` décrit une seule application :

```toml
format_version = 1
name = "api.neotamia"
command = ["bun", "run", "dev"]
tls = true
app_port = 5173
strict_port = false
readiness_warn_after_seconds = 30
```

Sans commande après `--`, `command` est obligatoire. Le nom suit la priorité : `--name`, fichier projet, basename de la racine Git, puis basename du répertoire courant. Les valeurs CLI remplacent celles du fichier.

## Configuration globale

Le fichier global est `$XDG_CONFIG_HOME/nook/config.toml`, avec fallback `~/.config/nook/config.toml` :

```toml
format_version = 1
caddy_admin = "http://127.0.0.1:2019"

# À définir seulement si la découverte est ambiguë.
# https_server = "https"
# http_server = "http"
```

Si l’Admin API de Caddy écoute sur un socket Unix, utilisez directement son adresse Caddy :

```toml
caddy_admin = "unix//run/caddy/admin.socket"
```

L’utilisateur qui exécute Nook doit avoir le droit de traverser le répertoire et de lire/écrire sur le socket. La forme URI `unix:///run/caddy/admin.socket` est également acceptée.

Pour un remplacement ponctuel sans modifier ce fichier, passez directement le chemin du socket :

```sh
nook --caddy-socket /run/caddy/admin.socket status
nook run --caddy-socket /run/caddy/admin.socket --name api --app-port 3000 -- command
```

L’option est globale, peut être placée avant ou après la sous-commande et prime sur `caddy_admin`.

L’état versionné réside dans `$XDG_STATE_HOME/nook/state.json`, avec fallback `~/.local/state/nook/state.json`. Les écritures sont atomiques et verrouillées ; il ne faut pas éditer ce registre pendant l’exécution de Nook.

## Dépannage

- `Caddy Admin API request failed` : vérifier que Caddy tourne, que `caddy_admin` est correct et, pour un socket Unix, que ses permissions autorisent l’utilisateur courant.
- `expected exactly one ... server; detected: none` : ajouter le listener `:443` ou `:80` correspondant dans Caddy.
- plusieurs serveurs compatibles détectés : utiliser les candidats affichés pour définir `https_server` ou `http_server`.
- `no selected Caddy HTTP server` : configurer un listener `:80` avant d’utiliser `--no-tls`.
- `hostname ... foreign Caddy route` : choisir un autre nom ou modifier cette route directement hors de Nook.
- `drift detected` ou cleanup en attente : lancer `nook prune`.
- `local_ca not trusted` : exécuter manuellement la commande `caddy trust --address ...` affichée. Nook ne l’exécute jamais.
- warning de readiness : vérifier que l’application écoute bien sur `HOST` et `PORT`; la route et le processus restent actifs.
- port strict occupé : libérer le port, en choisir un autre ou retirer `--strict-port`.

## Périmètre du MVP

Le MVP gère un seul service par projet, les routes Caddy locales, les aliases persistants, les processus Linux et leur récupération au prochain appel CLI.

Sont hors périmètre : daemon permanent, IPC ou socket local, shell implicite, modification de `/etc/hosts`, installation/démarrage de Caddy, installation automatique de CA, Docker, LAN/mDNS, plusieurs services ou workspaces, Windows/macOS, Tailscale Serve/Funnel et toute exposition publique.

## Développement

```sh
cargo fmt --check
cargo check
cargo test -- --test-threads=1
cargo clippy --all-targets --all-features -- -D warnings
```

Les intégrations requièrent Caddy `2.11.x`, OpenSSL, Python 3, curl avec HTTP/2, `unshare` et `ip`. Elles utilisent uniquement des ports loopback et des répertoires temporaires, désactivent l’installation de confiance et nettoient leurs processus et fichiers. Le test complet Nook/Caddy ouvre `:80` et `:443` dans un namespace réseau utilisateur isolé ; la CI effectue le même test sur son runner jetable.

La CI Linux applique cette porte avec la toolchain épinglée dans `rust-toolchain.toml`, puis produit un artefact x86-64 et sa somme SHA-256. Voir [la traçabilité](docs/TRACEABILITY.md) et [les notes de release](RELEASE.md).
