# Caddy in Docker

Nook can remain installed on the Linux or Windows host while Caddy 2.11 runs in a container. The
official image is the supported path. Nook does not control Docker and neither starts nor stops
Caddy. On Windows, native `caddy.exe` is the primary supported mode; Docker Desktop is a secondary
supported alternative.

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

## Windows with Docker Desktop (secondary mode)

Install Nook natively on Windows first. Then create its configuration and start the same official
Compose stack from PowerShell:

```powershell
$configDirectory = Join-Path $env:APPDATA "Nook"
New-Item -ItemType Directory -Force $configDirectory | Out-Null
Copy-Item docker/nook-config.windows.toml.example `
  (Join-Path $configDirectory "config.toml")
docker compose -f docker/compose.yaml up -d --wait
nook status
```

The Admin API remains published only on `127.0.0.1`. Caddy reaches Windows applications through
`host.docker.internal`. Because Docker Desktop cannot reach a process bound only to Windows
loopback in this bridge setup, the example sets `run_bind_address = "0.0.0.0"`. Keep Windows
Firewall enabled and do not add an inbound firewall rule for application ports. If the fixed
`172.30.0.0/24` subnet conflicts with another network, change the Compose subnet/gateway and
`caddy_client_ip_ranges` together.

This mode does not require the Caddy Windows service or `caddy.exe` on the host. It also does not
give Nook access to the Docker socket.

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

An explicit current-user import from PowerShell is:

```powershell
Import-Certificate -FilePath .\caddy-local-ca.pem `
  -CertStoreLocation Cert:\CurrentUser\Root
```

This is a deliberate user action; Nook never invokes it.

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
| Windows | native | native `caddy.exe` | supported; primary Windows path |
| Windows | native | official image in Docker Desktop | supported; secondary path |
| Windows | native | caddy-docker-proxy in Docker Desktop | not part of the Windows gate |
| Windows + WSL | Linux in WSL | Docker Desktop | compatibility path; native Nook is preferred |
| macOS | native | Docker Desktop | Caddy is feasible; Nook is not ported |

The Windows CI gate uses native `caddy.exe`. Docker Desktop validation supplements that gate and
never replaces it.
