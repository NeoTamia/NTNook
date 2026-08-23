# Traçabilité du MVP

Cette matrice relie les exigences produit aux tickets d’implémentation et aux vérifications suivies dans le dépôt. Les tickets post-MVP en attente ne font pas partie de la porte de sortie.

| Exigence vérifiable | Tickets | Test ou vérification |
| --- | --- | --- |
| Un crate binaire Rust Linux, modules internes et erreurs publiques cohérentes | NOOK-10, NOOK-32 | `cargo check`; `src/main.rs` et modules |
| CLI `run`, forme courte, arguments opaques et aide stable | NOOK-11, NOOK-32 | tests `cli::tests::*`; `cli_alias::help_is_successful_*` |
| Configuration globale/projet versionnée, gestion via `nook config` et précédence CLI → projet → défauts | NOOK-12 | tests `config::tests::*`, `cli::tests::parses_global_configuration_commands`, `cli_config` |
| Nom normalisé en label DNS avec fallback projet/Git/répertoire | NOOK-17 | `config::tests::name_priority_*`, `normalizes_valid_names`, `rejects_invalid_dns_labels` |
| Registre XDG versionné, sans argv, écrit atomiquement et verrouillé | NOOK-15, NOOK-16, NOOK-38 | tests `state::tests::*`, dont concurrence et récupération du fichier temporaire |
| Client Admin API sans lancement de Caddy et erreurs exploitables | NOOK-18, NOOK-32 | `caddy::tests::admin_client_*`; `cli_alias::status_has_a_stable_failure_*` |
| Découverte sûre des serveurs `:443`/`:80`, overrides et ambiguïtés | NOOK-18 | tests `discovers_*`, `available_server_*`, `ambiguity_*`, `no_tls_*` |
| Conteneurs Nook placés avant catch-all sans altérer les routes étrangères | NOOK-19, NOOK-20 | tests `containers_partition_*`, `container_is_repositioned_*`, `empty_container_*` |
| Remplacement atomique par `PATCH`, ETag avec retries bornés et relecture | NOOK-21, NOOK-41 | tests `managed_backend_*`, `retries_re_read_*`, `fourth_precondition_*`; vraie Admin API dans `nook_caddy_e2e` |
| Ownership UUID, cleanup conditionnel et protection contre un ancien propriétaire | NOOK-23, NOOK-26, NOOK-38 | tests `owner_marker_*`, `stale_owner_cleanup_*`, concurrence CLI |
| Matcher conjoint hostname + source loopback | NOOK-20, NOOK-41 | `proxy_route_combines_host_and_loopback_*`; test non-loopback `proxy_protocols` |
| Upstream port ou URL HTTP(S), validation stricte et TLS jamais désactivé | NOOK-28, NOOK-41 | tests de validation Caddy; `alias_tls` valide/expiré/non approuvé/hostname incorrect |
| Aliases persistants, formes courtes, suppression idempotente et `--force` limité à Nook | NOOK-26, NOOK-28, NOOK-29, NOOK-30, NOOK-38 | tests reconcile; `cli_alias::alias_shortcuts_*`, `force_refuses_a_foreign_*` |
| Allocation de port, `{port}`, environnement et absence de relance après course | NOOK-22, NOOK-38 | tests process `reserve_port`, `substitution`, `child_environment`, `lost_port_race_*` |
| Processus en groupe, readiness, warning, signaux et code de sortie conservé | NOOK-24, NOOK-25, NOOK-27, NOOK-32, NOOK-38 | tests process; intégrations SIGINT avant/après readiness, SIGTERM, stop/force et code de cleanup |
| Aucun enfant/lease/route orphelin après spawn impossible ou mort du superviseur | NOOK-25, NOOK-27, NOOK-38 | `failed_spawn_*`, `caddy_failure_before_run_*`, `prune_recovers_after_*` |
| Journaux transactionnels convergents à chaque frontière de mutation | NOOK-13, NOOK-16, NOOK-38 | `recovers_journals_left_at_every_external_mutation_boundary`; tests reconcile |
| Toute commande opérationnelle réconcilie d’abord ; sélection et horodatage sont persistés | NOOK-13, NOOK-33, NOOK-38 | `ordinary_list_reconciles_reload_and_records_synchronization`; reload Caddy réel dans `nook_caddy_e2e` |
| `list`, `status`, `stop`, `stop --force` et `prune` sûrs | NOOK-13, NOOK-25, NOOK-33, NOOK-36, NOOK-38 | tests CLI/process/reconcile, stop forcé réel et harness reload/restauration |
| Diagnostic de dérive et confiance CA sans exécuter de commande privilégiée | NOOK-36 | tests `status_drift_*`, `local_ca_diagnostic_*`; intégration CA non approuvée |
| Routes réellement produites par Nook : run/alias HTTPS et HTTP-only, reload, concurrence et protection étrangère | NOOK-38, NOOK-41 | `tests/nook_caddy_e2e.rs` sur vraie Admin API Caddy et ports `:80`/`:443` |
| Bind hôte configurable, traduction des upstreams loopback et plages clientes Docker | NOOK-45 | tests `docker_network_settings_*`, `docker_route_translates_*`, processus et `tests/docker_e2e.sh` |
| Export public et empreinte de la CA sans binaire Caddy | NOOK-46 | tests CLI `ca export`, validation E2E avec `curl --cacert` |
| Compose officiel sécurisé et volumes persistants | NOOK-47 | `docker/compose.yaml`, `docker/Caddyfile`, `docker compose config`, E2E restart/empreinte |
| Coexistence caddy-docker-proxy et restauration après reload | NOOK-48 | `docker/compose.caddy-docker-proxy.yaml`, scénario labels/réconciliation |
| Porte CI Docker officielle/proxy | NOOK-49 | job `docker` dans `.github/workflows/ci.yml` |
| Documentation Docker et matrice cross-platform | NOOK-51, NOOK-50 | `docs/DOCKER.md`, README, release et spécification YouTrack |
| Host préservé, forwarded headers, WebSocket, SSE, streaming, HTTP/2, 502 et TLS upstream | NOOK-41 | `tests/proxy_protocols.rs`; `tests/alias_tls.rs` |
| Documentation des prérequis, garde-fous, dépannage et hors-périmètre | NOOK-31, NOOK-43 | `README.md`, `RELEASE.md` |
| Porte Linux compile/format/lint/tests/intégrations et produit un binaire vérifiable | NOOK-34, NOOK-35, NOOK-38, NOOK-41, NOOK-43 | `.github/workflows/ci.yml`; archive, SHA-256 et attestation via `.github/workflows/publish.yml` |

## Porte de sortie

La validation locale et CI exécute, dans cet ordre :

```sh
cargo fmt --check
cargo check --locked
cargo test --locked -- --test-threads=1
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo build --locked --release
```

Les intégrations sont isolées dans des répertoires temporaires, utilisent Caddy `2.11.x`, n’installent aucune CA et nettoient leurs processus. La release MVP exige que tous les tickets MVP reliés soient résolus ; les travaux Tailscale restent explicitement post-MVP et en attente.
