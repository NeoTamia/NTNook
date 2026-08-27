# Caddy in Docker

Nook can remain installed on the Linux host while Caddy 2.11 runs in a container. The official image is the supported path. Nook does not control Docker and neither starts nor stops Caddy.

## Recommended setup

Docker Engine and Docker Compose v2 are required. First verify that `172.30.0.0/24` does not conflict with an existing network, then copy the Nook configuration:

```sh
mkdir -p "${XDG_CONFIG_HOME:-$HOME/.config}/nook"
cp docker/nook-config.toml.example "${XDG_CONFIG_HOME:-$HOME/.config}/nook/config.toml"
docker compose -f docker/compose.yaml up -d --wait
nook status
```

The Compose setup publishes HTTP, HTTPS, HTTP/3, and the Admin API only on `127.0.0.1`. Inside the bridge, Caddy reaches host applications through `host.docker.internal`, explicitly mapped to this network's `172.30.0.1` gateway rather than to the default Docker bridge gateway.

Applications launched by Nook listen on this gateway, not on `0.0.0.0`. Caddy routes accept only requests whose source, as seen by Caddy, is `172.30.0.1/32`.

If the subnet must change, update the Compose `subnet` and `gateway`, `run_bind_address`, and `caddy_client_ip_ranges` together.

## Trust HTTPS

Caddy stores its PKI in the named `caddy_data` volume. Export its public certificate once:

```sh
nook ca export caddy-local-ca.pem
```

Nook prints the SHA-256 fingerprint but never installs the certificate. On Debian/Ubuntu, an explicit installation looks like this:

```sh
sudo cp caddy-local-ca.pem /usr/local/share/ca-certificates/nook-caddy.crt
sudo update-ca-certificates
```

On Windows, after verifying the displayed fingerprint, import the PEM into “Trusted Root Certification Authorities” for the current user. Some browsers use their own certificate store and require a separate import.

You do not need to export the CA again after a restart or recreation that preserves `caddy_data`. Export it again if the volume is deleted or replaced, the PKI is regenerated, or the Caddy instance changes.

## Security

Caddy's Admin API can modify the configuration without application-level authentication. Do not change the `127.0.0.1:…` port publications to `0.0.0.0:…`, and do not expose port 2019 to the LAN.

Nook does not read the Docker socket. The official image does not need it.

## caddy-docker-proxy

The following variant is compatibility-tested but is not the primary path:

```sh
docker compose -f docker/compose.caddy-docker-proxy.yaml up -d --wait
```

It mounts `/var/run/docker.sock`. The `:ro` suffix prevents direct writes to the file, but the daemon API remains highly privileged.

The plugin rebuilds and reloads a Caddyfile for every relevant Docker event. A route added dynamically by Nook can therefore disappear until the next operational command (`status`, `prune`, and so on) reconciles it. The compatibility test covers this interruption.

## Tests

The tests destroy only their own Compose project and dedicated volumes:

```sh
NOOK_DOCKER_E2E=1 tests/docker_e2e.sh
NOOK_DOCKER_E2E=1 \
  NOOK_DOCKER_COMPOSE="$PWD/docker/compose.caddy-docker-proxy.yaml" \
  tests/docker_e2e.sh
```

## Platforms

| Host | Nook | Caddy | Status |
|---|---|---|---|
| Linux | native | native | supported |
| Linux | native | official Docker image | supported |
| Linux | native | caddy-docker-proxy | compatibility-tested with reconciliation |
| Windows | native | Docker Desktop | Caddy is feasible; Nook is not ported |
| Windows + WSL | Linux in WSL | Docker Desktop | exploratory; not guaranteed |
| macOS | native | Docker Desktop | Caddy is feasible; Nook is not ported |

Porting Nook to Windows remains a separate effort: `/proc`, process groups, POSIX signals, Unix sockets, XDG paths, and trust-store detection must be replaced or made conditional.
