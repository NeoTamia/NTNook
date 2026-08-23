# Nook

Nook est une CLI Linux qui expose des applications locales sous des domaines stables `*.localhost` en configurant une instance Caddy existante.

## Prérequis et installation

- Linux ;
- Caddy `2.11.x`, natif ou dans Docker, déjà démarré et accessible par son Admin API ;
- des serveurs Caddy non ambigus écoutant sur `:443` pour HTTPS et, si `--no-tls` est utilisé, sur `:80` pour HTTP.

Installation recommandée du binaire précompilé Linux x86-64 :

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/NeoTamia/NTNook/releases/latest/download/nook-installer.sh | sh
nook --help
```

Le script installe Nook dans `$XDG_BIN_HOME`, ou `~/.local/bin` par défaut, sans utiliser `sudo`.
`NOOK_INSTALL_DIR` permet de choisir un autre répertoire et `NOOK_VERSION` d'installer une version
précise. Les utilisateurs de Rust peuvent également compiler la version publiée sur crates.io :

```sh
cargo install ntnook --locked
```

Pour compiler le dépôt localement :

```sh
caddy version
cargo install --path .
nook --help
```

Nook ne démarre ni n’installe Caddy. Il ne lance jamais `sudo`, ne modifie pas `/etc/hosts` et n’installe pas la CA locale. Les noms sous `.localhost` sont résolus nativement vers loopback par les navigateurs et systèmes compatibles.

Pour exécuter Caddy dans Docker sans installer son binaire sur l’hôte, utilisez le [guide Docker](docs/DOCKER.md). L’image officielle est supportée ; `caddy-docker-proxy` fait l’objet d’un test de compatibilité avec une réserve sur ses reloads.

## Complétion Bash et Zsh

Nook génère des scripts de complétion synchronisés avec les commandes et options de la version
installée. Pour les charger uniquement dans la session courante :

```sh
# Bash
source <(nook completions bash)

# Zsh
autoload -Uz compinit
compinit
source <(nook completions zsh)
```

Pour une installation Bash persistante :

```sh
completion_dir="${XDG_DATA_HOME:-$HOME/.local/share}/bash-completion/completions"
mkdir -p "$completion_dir"
nook completions bash > "$completion_dir/nook"
```

Pour Zsh, générez `_nook` dans un répertoire de fonctions :

```sh
completion_dir="${XDG_DATA_HOME:-$HOME/.local/share}/zsh/site-functions"
mkdir -p "$completion_dir"
nook completions zsh > "$completion_dir/_nook"
```

Ajoutez ensuite ce répertoire à `fpath` dans `.zshrc`, avant l’appel à `compinit` :

```zsh
fpath=("${XDG_DATA_HOME:-$HOME/.local/share}/zsh/site-functions" $fpath)
autoload -Uz compinit
compinit
```

Régénérez le fichier après chaque mise à jour de Nook. Cette première version complète les formes
canoniques, comme `nook run --name api` et `nook alias set api 3000`. Les raccourcis
`nook api run` et `nook alias api 3000`, ainsi que les noms de runs ou aliases existants, ne sont
pas encore complétés dynamiquement.

## Préparer Caddy pour Nook

Nook ajoute ses routes à un serveur Caddy existant : il ne crée pas les listeners lui-même. Pour les routes HTTPS, le Caddyfile doit produire exactement un serveur écoutant explicitement sur `:443`. Par exemple, ajoutez ce site à votre configuration existante :

```caddyfile
https://localhost {
	tls internal
	respond 404
}
```

Si toutes les commandes utilisent `--no-tls`, aucun serveur HTTPS n’est requis. Caddy doit alors fournir exactement un serveur HTTP sur `:80` ; Nook n’émet ni ne vérifie de certificat pour ces routes.

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
- `--local` applique le `nook.local.toml` voisin d'un fichier choisi avec `--config` ;
- `--readiness-warn-after <seconds>` règle le délai du warning de readiness ;
- les arguments après `--` sont transmis directement, sans shell implicite.

Nook remplace littéralement `{port}` dans chaque argument et injecte `PORT`, `HOST` (la valeur de `run_bind_address`, `127.0.0.1` par défaut) et `NOOK_URL`. Le processus reçoit stdin/stdout/stderr du terminal et son code de sortie est conservé, même si le cleanup Caddy doit être réessayé plus tard.

Après la réservation de la route et le lancement du processus, Nook affiche toujours le domaine, l’URL publique et le port applicatif effectivement retenus, y compris lorsque le nom et le port sont inférés :

```text
nook: domain=api.localhost url=https://api.localhost port=5173
```

Cette information est écrite sur stderr afin de ne pas mélanger les messages de supervision avec la sortie standard de l’application.

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

Chaque développeur peut ajouter un `nook.local.toml` dans le même répertoire. Ses champs
remplacent ceux de `nook.toml` sans modifier la configuration partagée :

```toml
format_version = 1
name = "api-alwyn"
app_port = 5180
strict_port = true
```

La priorité complète est : valeurs par défaut et inférence, `nook.toml`, `nook.local.toml`, puis
options CLI. Le fichier local peut aussi être utilisé seul, sans `nook.toml`. Chaque fichier est
validé séparément, doit déclarer `format_version = 1` et refuse les champs inconnus.

Ce fichier étant propre au poste, ajoutez-le au `.gitignore` du projet :

```gitignore
/nook.local.toml
```

Nook ne modifie pas `.gitignore` et ne vérifie pas si le fichier est suivi par Git.

`--config chemin/custom.toml` reste déterministe et ne charge que le fichier demandé. Si un
`nook.local.toml` existe à côté, Nook signale qu'il est ignoré. Ajoutez explicitement `--local`
pour le superposer :

```sh
nook run --config chemin/custom.toml --local
```

Dans ce mode, `--local` échoue si le fichier voisin est absent et ne peut pas être utilisé sans
`--config`.

### Fallback lorsque Nook n'est pas installé

Pour qu'un script `dev` reste utilisable par un développeur qui n'a pas encore Nook, séparez la
commande applicative brute et testez le binaire directement dans `dev`. Exemple avec pnpm :

```json
{
  "scripts": {
    "dev": "if command -v nook >/dev/null 2>&1; then exec nook run; else printf '%s\\n' 'warning: Nook is not installed; starting without the local domain proxy' >&2; exec pnpm run dev:app; fi",
    "dev:app": "vite"
  }
}
```

Le `nook.toml` partagé référence alors la commande brute :

```toml
format_version = 1
name = "app"
command = ["pnpm", "run", "dev:app"]
```

Pour npm, Yarn ou Bun, remplacez les deux occurrences de `pnpm` par respectivement `npm`, `yarn`
ou `bun`. Le fallback ne s'exécute que si le binaire est absent : une erreur de Nook, de Caddy ou
de l'application conserve son code de sortie et ne relance pas le serveur hors proxy. Cette
recette utilise le shell POSIX, comme Nook est actuellement limité à Linux.

## Configuration globale

Le fichier global est `$XDG_CONFIG_HOME/nook/config.toml`, avec fallback `~/.config/nook/config.toml` :

Nook peut le créer, afficher sa configuration effective et modifier une valeur :

```sh
nook config init
nook config init --caddy-socket /run/caddy/admin.socket
nook config show
nook config path
nook config set caddy-admin unix:///run/caddy/admin.socket
```

`config init` refuse d’écraser un fichier existant sans `--force`. `config set` accepte les clés
`caddy-admin`, `https-server`, `http-server`, `run-bind-address`, `caddy-loopback-host` et
`caddy-client-ip-ranges`. Utilisez `auto` comme valeur d’un serveur pour supprimer son override,
et séparez plusieurs plages IP par des virgules.
`config show` affiche la configuration effective en ajoutant les valeurs par défaut des champs
absents. `config path` affiche uniquement le chemin du fichier brut, ce qui permet par exemple
`bat "$(nook config path)"`.

```toml
format_version = 1
caddy_admin = "http://127.0.0.1:2019"
run_bind_address = "127.0.0.1"
caddy_loopback_host = "127.0.0.1"
caddy_client_ip_ranges = ["127.0.0.0/8", "::1"]

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
nook --caddy-socket /run/caddy/admin.socket run --name api --app-port 3000 -- command
```

Pour les commandes opérationnelles, l’option peut se placer avant ou après la sous-commande et
prime ponctuellement sur `caddy_admin`. Pour enregistrer le socket dans la configuration, utilisez
`nook config init --caddy-socket PATH` ou
`nook config set caddy-admin unix:///chemin/admin.socket`.

`run_bind_address` choisit l’interface utilisée pour réserver le port, sonder la readiness et injecter `HOST`. `caddy_loopback_host` remplace uniquement l’adresse de connexion des upstreams locaux vus par Caddy. `caddy_client_ip_ranges` contrôle le matcher `remote_ip` ajouté à chaque route Nook. Les valeurs par défaut conservent le comportement natif loopback.

## Exporter la CA locale

Lorsque Caddy n’est pas installé sur l’hôte, exportez son certificat public via l’Admin API :

```sh
nook ca export caddy-local-ca.pem
nook ca export caddy-local-ca.pem --force
```

Nook affiche l’empreinte SHA-256, refuse l’écrasement par défaut et n’installe jamais le certificat. Avec Caddy dans Docker, la CA reste stable tant que le volume `/data` est conservé.

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

Sont hors périmètre : daemon permanent, IPC ou socket local, shell implicite, modification de `/etc/hosts`, installation/démarrage de Caddy, installation automatique de CA, orchestration du cycle de vie Docker, LAN/mDNS, plusieurs services ou workspaces, Windows/macOS natifs, Tailscale Serve/Funnel et toute exposition publique.

## Développement

```sh
cargo fmt --check
cargo check
cargo test -- --test-threads=1
cargo clippy --all-targets --all-features -- -D warnings
```

Les intégrations requièrent Caddy `2.11.x`, OpenSSL, Python 3, curl avec HTTP/2, `unshare` et `ip`. Elles utilisent uniquement des ports loopback et des répertoires temporaires, désactivent l’installation de confiance et nettoient leurs processus et fichiers. Le test complet Nook/Caddy ouvre `:80` et `:443` dans un namespace réseau utilisateur isolé ; la CI effectue le même test sur son runner jetable.

La CI Linux applique cette porte avec la toolchain épinglée dans `rust-toolchain.toml`. Chaque tag
`v*` produit ensuite un binaire statique Linux x86-64, sa somme SHA-256 et une attestation GitHub,
puis publie le paquet source `ntnook` sur crates.io. Voir
[la traçabilité](docs/TRACEABILITY.md) et [les notes de release](RELEASE.md).
