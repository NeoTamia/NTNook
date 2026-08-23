# Changelog

## [0.3.0](https://github.com/NeoTamia/NTNook/compare/v0.2.0...v0.3.0) (2026-08-23)


### ✨ Features

* **cli:** Add bash and zsh completions ([4971178](https://github.com/NeoTamia/NTNook/commit/49711787de7a481cf5a47927d07a6c46c185d1e8))


### 🐛 Bug Fixes

* **ci:** Publish crate from clean checkout ([7086332](https://github.com/NeoTamia/NTNook/commit/708633243d31b243625110c6476a9184cd2b36b2))
* **cli:** Address completion review feedback ([ce3995c](https://github.com/NeoTamia/NTNook/commit/ce3995c869391637fd6a4ec0688fdf5d5e13c9ef))


### 🔧 Build System

* **deps:** Update actions/attest digest to 1e69f48 ([e94a3a2](https://github.com/NeoTamia/NTNook/commit/e94a3a20654d654a43567d9e383619c0ac628988))
* **deps:** Update rust crate signal-hook to 0.4.0 ([#8](https://github.com/NeoTamia/NTNook/issues/8)) ([66cae6e](https://github.com/NeoTamia/NTNook/commit/66cae6e32b469febb22e013a2aff651c45324adb))

## [0.2.0](https://github.com/NeoTamia/NTNook/compare/v0.1.0...v0.2.0) (2026-08-23)


### ✨ Features

* Add Caddy socket CLI override ([9a80b91](https://github.com/NeoTamia/NTNook/commit/9a80b9157516502f39703173c35419184a3840da))
* Build Caddy route containers ([b541bad](https://github.com/NeoTamia/NTNook/commit/b541bad230b83332bc66e4486d279ea685566a5f))
* **ca:** Export and fingerprint the Caddy local CA ([faa24d1](https://github.com/NeoTamia/NTNook/commit/faa24d176208495004def30c2e796eaf70ee2885))
* **config:** Add global configuration commands ([3163072](https://github.com/NeoTamia/NTNook/commit/3163072675ce00547cd2010e8ab8f4ec486164d4))
* **config:** Make process bind and Caddy upstream networks configurable ([faa24d1](https://github.com/NeoTamia/NTNook/commit/faa24d176208495004def30c2e796eaf70ee2885))
* Define versioned state registry ([571ea10](https://github.com/NeoTamia/NTNook/commit/571ea103c6fff5daf0d8863ff4983a1bdaea2bf7))
* Diagnose Caddy drift and local CA trust ([2a6704d](https://github.com/NeoTamia/NTNook/commit/2a6704dcce53475c534960c5de960eca5657dc5c))
* Discover Caddy admin servers ([b0e9824](https://github.com/NeoTamia/NTNook/commit/b0e98249853f52b9bca850139082aba932ad94e7))
* **docker:** Support containerized Caddy ([faa24d1](https://github.com/NeoTamia/NTNook/commit/faa24d176208495004def30c2e796eaf70ee2885))
* Execute persistent alias commands ([8020215](https://github.com/NeoTamia/NTNook/commit/8020215a6ccc15c5e81dd6ef02adb20f1fc8cda4))
* Execute runs and unify CLI failures ([b67654b](https://github.com/NeoTamia/NTNook/commit/b67654b86f3cd3acb833107a8e9514dae090f8c7))
* Finalize run cleanup and stop ([30b1a1a](https://github.com/NeoTamia/NTNook/commit/30b1a1a018e768c7b0473439e6f74d113fdad32d))
* Identify Linux process leases ([ee2646b](https://github.com/NeoTamia/NTNook/commit/ee2646bb567df6a9786b1c3f4072a6627d822b38))
* Implement CLI contract ([90f8c0a](https://github.com/NeoTamia/NTNook/commit/90f8c0adeef393d63dce3d27d848a3100b026818))
* Implement operational status and prune ([02f73a5](https://github.com/NeoTamia/NTNook/commit/02f73a59a502f4efe8be95b341188c74e6fe384c))
* Initialize Rust crate architecture ([8492932](https://github.com/NeoTamia/NTNook/commit/8492932909411f40fb1fa60e2dbc0bf3b4c496c7))
* Load configuration and normalize hostnames ([de3b3f2](https://github.com/NeoTamia/NTNook/commit/de3b3f266773c14ccb241ac9d26333e52333d13f))
* Mark Caddy routes with lease ownership ([b42028d](https://github.com/NeoTamia/NTNook/commit/b42028d788970f3e5759684bb0aac4912eef5d77))
* Mutate Caddy routes with ETag retries ([db6bfc2](https://github.com/NeoTamia/NTNook/commit/db6bfc2bc433caa7628d22c7a787e50c219367b7))
* Orchestrate recoverable run startup ([26d2a06](https://github.com/NeoTamia/NTNook/commit/26d2a064d304a026dcf769e12c3343526a74c82a))
* Persist aliases and configure proxy headers ([de4576e](https://github.com/NeoTamia/NTNook/commit/de4576ebf9b150522f991257a62bfc547df5fd05))
* Persist recoverable run transitions ([90ae88c](https://github.com/NeoTamia/NTNook/commit/90ae88c5b32a424808e7cc1de51971b1a10ef083))
* Persist state with atomic locking ([b04ff65](https://github.com/NeoTamia/NTNook/commit/b04ff650bb9cd881e144fb2dbacc8946ea3fe4ae))
* Protect Nook routes from foreign traffic ([439ef07](https://github.com/NeoTamia/NTNook/commit/439ef075e4c786aa31c41db62c9fff58bf28fe01))
* Reconcile leases and recovery operations ([58d011e](https://github.com/NeoTamia/NTNook/commit/58d011e312b959b3968d74be73e6c6206a8f51e3))
* Reconcile owned routes through Caddy ([237d130](https://github.com/NeoTamia/NTNook/commit/237d13010e969ed05ce8bb42483701494985375a))
* Report run domain and effective port ([235110d](https://github.com/NeoTamia/NTNook/commit/235110d19933850916a4b248845348e56688ca59))
* Reserve run ports and prepare child input ([ce1272f](https://github.com/NeoTamia/NTNook/commit/ce1272f8f98cb3774e1238f29cd7b99cdfc9493b))
* Supervise Linux process groups ([f674e25](https://github.com/NeoTamia/NTNook/commit/f674e25b234ee987f825acb6485a5eaee1f00fbc))
* Support Caddy admin Unix sockets ([4a7d704](https://github.com/NeoTamia/NTNook/commit/4a7d704370736b9136ec19db448d3924d5e19073))
* Transfer forced route ownership ([ac2baaa](https://github.com/NeoTamia/NTNook/commit/ac2baaab8d98cf4f5ce029cd323a46dd2ec67d74))
* Validate alias upstream targets ([12a8ba3](https://github.com/NeoTamia/NTNook/commit/12a8ba394bd6e750e6a5e85c51278e937ed4b039))


### 🐛 Bug Fixes

* Clarify missing Caddy listener errors ([c209110](https://github.com/NeoTamia/NTNook/commit/c2091105c283dc64f08bdf1552da64d96dda424b))
* Enforce MVP reconciliation and real Caddy integration ([0bf4e68](https://github.com/NeoTamia/NTNook/commit/0bf4e68961f3ef6c5405d67ae565d344a9e2f2e7))
* Migrate sha2 to 0.11.0 ([54bee3b](https://github.com/NeoTamia/NTNook/commit/54bee3beb2f7bb887a9f394d3dd9f3550da7f462))


### 📚 Documentation

* **docker:** Document setup, security, persistence, and platform limits ([faa24d1](https://github.com/NeoTamia/NTNook/commit/faa24d176208495004def30c2e796eaf70ee2885))
* Explain Caddy HTTPS and local CA setup ([6ca5e5f](https://github.com/NeoTamia/NTNook/commit/6ca5e5f1062d7baad0b89fe9e3f1a41a8a66eb26))
* Explain MVP usage and safeguards ([a4d5833](https://github.com/NeoTamia/NTNook/commit/a4d58334b2820cf550752c525ec9da37e21bc4df))
* Explain persistent Caddy group access ([5dd52fb](https://github.com/NeoTamia/NTNook/commit/5dd52fbe6d44305f1f972d226c25dc9210083bbb))


### 🧪 Tests

* Add isolated Caddy integration harness ([e361309](https://github.com/NeoTamia/NTNook/commit/e361309793bd80e04903ceadccf2d3b52b3e9576))
* Cover HTTP-only Caddy with no TLS ([3b1d802](https://github.com/NeoTamia/NTNook/commit/3b1d802b406bf147fb24a9f917ba1ac1b1ac284d))
* Cover lifecycle recovery and concurrency ([3f16610](https://github.com/NeoTamia/NTNook/commit/3f166104d15f926f9542fd163cc6f239f47909ea))
* **docker:** Cover official Caddy and caddy-docker-proxy in E2E and CI ([faa24d1](https://github.com/NeoTamia/NTNook/commit/faa24d176208495004def30c2e796eaf70ee2885))
* Handle partial Unix socket requests ([013468a](https://github.com/NeoTamia/NTNook/commit/013468ad6f20e7fd5cca459738f06bf16d9b3ca0))
* Make expired TLS fixture portable ([#2](https://github.com/NeoTamia/NTNook/issues/2)) ([e286eff](https://github.com/NeoTamia/NTNook/commit/e286eff033073b47365a56bdffa0f81c0bdd345a))
* Validate proxy protocols end to end ([7accdab](https://github.com/NeoTamia/NTNook/commit/7accdab04a1f3d53d951c571db811f29438765aa))


### 🔧 Build System

* Add Renovate configuration ([b14d60b](https://github.com/NeoTamia/NTNook/commit/b14d60b29f8817504e5a3e34bc4fea372a900ea7))
* Add Renovate configuration ([60cd5d1](https://github.com/NeoTamia/NTNook/commit/60cd5d1eaec08a3adf363e7e3a6c02fe59ce06c9))
* **deps:** Migrate sha2 to 0.11.0 ([c7fcfa8](https://github.com/NeoTamia/NTNook/commit/c7fcfa810b62dcfc075c7c9d2a01c37c3ec85800))
* **deps:** Pin dependencies ([19a1bad](https://github.com/NeoTamia/NTNook/commit/19a1badfd6fd049cc85078f33d75ab42f61d9716))
* **deps:** Update rust crate uuid to v1.25.0 ([d98a9dc](https://github.com/NeoTamia/NTNook/commit/d98a9dc7dbc4b46703bccfaca3f0a8f12e94111e))
* **deps:** Update rust crate uuid to v1.25.0 ([79471e8](https://github.com/NeoTamia/NTNook/commit/79471e8004b21ba566596f09dcc290586149b7a7))


### 👷 Continuous Integration

* Automate Nook distribution ([cae3438](https://github.com/NeoTamia/NTNook/commit/cae3438cd0b3ffb135bf8b77a76f5b487b78bd2c))
* Configure release please ([2e864bc](https://github.com/NeoTamia/NTNook/commit/2e864bcd2b8c5e0e3e7a387e15c696a1a093a9fd))
* Prepare reproducible Linux MVP release ([434770b](https://github.com/NeoTamia/NTNook/commit/434770b6c3af63c7c3cb2e77b1949002d6285577))
* Refresh actions and traceability ([#3](https://github.com/NeoTamia/NTNook/issues/3)) ([8b90ef9](https://github.com/NeoTamia/NTNook/commit/8b90ef9cee5f4ed36d566fec5c81974816b5f576))
