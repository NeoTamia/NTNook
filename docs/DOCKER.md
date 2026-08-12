# Caddy dans Docker

Nook peut rester installé sur l’hôte Linux tandis que Caddy 2.11 tourne dans un conteneur. L’image officielle est la voie supportée. Nook ne pilote pas Docker et ne démarre ni n’arrête Caddy.

## Démarrage recommandé

Docker Engine et Docker Compose v2 sont requis. Vérifiez d’abord que `172.30.0.0/24` n’entre pas en conflit avec un réseau existant, puis copiez la configuration Nook :

```sh
mkdir -p "${XDG_CONFIG_HOME:-$HOME/.config}/nook"
cp docker/nook-config.toml.example "${XDG_CONFIG_HOME:-$HOME/.config}/nook/config.toml"
docker compose -f docker/compose.yaml up -d --wait
nook status
```

Le Compose publie HTTP, HTTPS, HTTP/3 et l’Admin API uniquement sur `127.0.0.1`. À l’intérieur du bridge, Caddy joint les applications hôte via `host.docker.internal`, associé explicitement à la passerelle `172.30.0.1` de ce réseau plutôt qu’à la passerelle du bridge Docker par défaut.

Les applications lancées par Nook écoutent sur cette passerelle, pas sur `0.0.0.0`. Les routes Caddy n’acceptent que les requêtes dont la source vue par Caddy est `172.30.0.1/32`.

Si le sous-réseau doit changer, modifiez ensemble le subnet et la gateway du Compose, `run_bind_address` et `caddy_client_ip_ranges`.

## Faire confiance à HTTPS

Caddy conserve sa PKI dans le volume nommé `caddy_data`. Exportez une fois son certificat public :

```sh
nook ca export caddy-local-ca.pem
```

Nook affiche l’empreinte SHA-256 mais n’installe jamais le certificat. Sous Debian/Ubuntu, l’installation explicite ressemble à :

```sh
sudo cp caddy-local-ca.pem /usr/local/share/ca-certificates/nook-caddy.crt
sudo update-ca-certificates
```

Sous Windows, importez le PEM dans « Autorités de certification racines de confiance » pour l’utilisateur courant, après avoir vérifié l’empreinte affichée. Certains navigateurs utilisent leur propre magasin et nécessitent un import séparé.

Il n’est pas nécessaire de réexporter la CA après un redémarrage ou une recréation conservant `caddy_data`. Réexportez-la si le volume est supprimé/remplacé, si la PKI est régénérée ou si l’instance Caddy change.

## Sécurité

L’Admin API Caddy permet de modifier la configuration sans authentification applicative. Ne remplacez pas les publications `127.0.0.1:…` par `0.0.0.0:…` et n’exposez pas le port 2019 au LAN.

Nook ne lit pas le socket Docker. L’image officielle n’en a pas besoin.

## caddy-docker-proxy

La variante suivante est testée comme compatibilité, pas comme voie principale :

```sh
docker compose -f docker/compose.caddy-docker-proxy.yaml up -d --wait
```

Elle monte `/var/run/docker.sock`. Le suffixe `:ro` empêche les écritures directes dans le fichier, mais l’API du daemon reste très privilégiée.

Le plugin reconstruit puis recharge un Caddyfile à chaque événement Docker pertinent. Une route ajoutée dynamiquement par Nook peut donc disparaître jusqu’à la prochaine commande opérationnelle (`status`, `prune`, etc.), qui la réconcilie. Cette interruption est couverte par le test de compatibilité.

## Tests

Les tests détruisent uniquement leur projet Compose et leurs volumes dédiés :

```sh
NOOK_DOCKER_E2E=1 tests/docker_e2e.sh
NOOK_DOCKER_E2E=1 \
  NOOK_DOCKER_COMPOSE="$PWD/docker/compose.caddy-docker-proxy.yaml" \
  tests/docker_e2e.sh
```

## Plateformes

| Hôte | Nook | Caddy | Statut |
|---|---|---|---|
| Linux | natif | natif | supporté |
| Linux | natif | image officielle Docker | supporté |
| Linux | natif | caddy-docker-proxy | compatibilité testée avec réconciliation |
| Windows | natif | Docker Desktop | Caddy faisable, Nook non porté |
| Windows + WSL | Linux dans WSL | Docker Desktop | étude, non garanti |
| macOS | natif | Docker Desktop | Caddy faisable, Nook non porté |

Le portage Windows de Nook reste distinct : `/proc`, les groupes de processus, les signaux POSIX, les sockets Unix, les chemins XDG et la détection du trust store doivent être remplacés ou conditionnés.
