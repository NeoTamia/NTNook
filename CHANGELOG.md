# Changelog

## [0.11.0](https://github.com/NeoTamia/NTNook/compare/v0.10.0...v0.11.0) (2026-10-30)


### ✨ Features

* **worktree:** Configure isolated names from files, CLI and init ([caf0645](https://github.com/NeoTamia/NTNook/commit/caf0645703b5145c85cad2af6d48847ed6855de3))
* **worktree:** Preserve run ownership and isolate workspace LAN access ([fe0692e](https://github.com/NeoTamia/NTNook/commit/fe0692ea03aa4a82947b562901c9d71ada41054b))


### 🐛 Bug Fixes

* **workspace:** Improve runner diagnostics and group visibility ([caa1e9a](https://github.com/NeoTamia/NTNook/commit/caa1e9ac8ff42138a894fa9b3ff622cf32e9fded))
* **workspace:** Reject colliding LAN names before preparing routes ([8a49d7b](https://github.com/NeoTamia/NTNook/commit/8a49d7bfa132765fd3d948d11e17d1d88bc79702))


### 📚 Documentation

* **worktree:** Explain naming, overrides and private network access ([3f867df](https://github.com/NeoTamia/NTNook/commit/3f867dfb6826d9e93898bc99af4684de31fa5cf2))

## [0.10.0](https://github.com/NeoTamia/NTNook/compare/v0.9.0...v0.10.0) (2026-10-26)


### ✨ Features

* **workspace:** Join prepared apps from Turbo and Lerna scripts ([2017d65](https://github.com/NeoTamia/NTNook/commit/2017d65960ae5a88c1103b1b47822854de229f25))
* **workspace:** Run multiple apps with shared lifecycle ([54bc9c0](https://github.com/NeoTamia/NTNook/commit/54bc9c0c2a5c8fff9e89f671f62f367c2a9018e6))
* **workspace:** Validate additional monorepo runners ([0e615d0](https://github.com/NeoTamia/NTNook/commit/0e615d077d40124d43b793631d50b33026e19fdb))


### 🐛 Bug Fixes

* **process:** Tolerate owned groups exiting before signal delivery ([68fae63](https://github.com/NeoTamia/NTNook/commit/68fae634852c6ec9415fe8ae26578a0b97c80aec))
* **run:** Claim the TTY before exec and keep wrapper SIGTTOU ignored ([d4a070c](https://github.com/NeoTamia/NTNook/commit/d4a070cb0e5798cfbaf28d6e35d7e163110b4c6c))
* **run:** Claim the TTY on fg and ignore ptrace stops ([43f7bf8](https://github.com/NeoTamia/NTNook/commit/43f7bf8589cb6a500a619857ae97e6435b7d1706))
* **run:** Claim the TTY only when nook owns the foreground ([9152dc3](https://github.com/NeoTamia/NTNook/commit/9152dc3b4b61982e2a79f236629cb022f264a96f))
* **run:** Do not steal the TTY from a child's descendant ([a2e85a3](https://github.com/NeoTamia/NTNook/commit/a2e85a38dc118a940709a929d7353857b630abb6))
* **run:** Give the child the terminal so raw-mode CLIs keep running ([e2c199c](https://github.com/NeoTamia/NTNook/commit/e2c199c80e786a598d743905412b4b149607472d))
* **run:** Stop the wrapper process group on child SIGTSTP ([c38d1ed](https://github.com/NeoTamia/NTNook/commit/c38d1eda33987920d6715b15470a5fa2edc79c76))
* **run:** Suspend nook when the child is job-control stopped ([8307123](https://github.com/NeoTamia/NTNook/commit/8307123f8ec99185d5657883c2966ee83fb80b71))
* **workspace:** Allow graceful runner shutdown before killing trees ([086bf8f](https://github.com/NeoTamia/NTNook/commit/086bf8f37e9795b6172e6d4cf4469ac0e623148e))
* **workspace:** Gracefully stop detached member descendants ([4c902a3](https://github.com/NeoTamia/NTNook/commit/4c902a30d08f03697c4a68faac5db68f2f60d613))
* **workspace:** Handle exited owners during environment rechecks ([61078ea](https://github.com/NeoTamia/NTNook/commit/61078eaba970ea2d155b3cd81ca809fdea17b6fd))
* **workspace:** Preserve runner terminal and member shutdown grace ([c0fa4c7](https://github.com/NeoTamia/NTNook/commit/c0fa4c7b16e9ed5de4cb473ce7459b4d907430c0))
* **workspace:** Preserve the native shutdown signal through cleanup ([9a90db4](https://github.com/NeoTamia/NTNook/commit/9a90db4250634182f5a6bd208c5b1183dc35c5cc))
* **workspace:** Protect pending apps and handle exited namespace reads ([44891d4](https://github.com/NeoTamia/NTNook/commit/44891d49a419f31b4c226c8a076990cb1c5b9fa4))
* **workspace:** Recover runner descendants and early sessions ([ab43e18](https://github.com/NeoTamia/NTNook/commit/ab43e18d8554f145c5cab3061efaf464dacb3c2e))
* **workspace:** Recover sessions across temp changes and detached children ([a4805e0](https://github.com/NeoTamia/NTNook/commit/a4805e0e2a774845dc687efa8c77cd1fb5fea4b5))
* **workspace:** Report precise recovery inspection failures ([252ab67](https://github.com/NeoTamia/NTNook/commit/252ab67e5a796613230b5434486ae2d871e1aad3))
* **workspace:** Revalidate exiting processes after proc permission races ([2227576](https://github.com/NeoTamia/NTNook/commit/22275766361fa40d98c1e0e64e29935ab45aa9d3))
* **workspace:** Signal native descendants before shutdown grace ([b47fa64](https://github.com/NeoTamia/NTNook/commit/b47fa64a62262b275e3e87e106d01a39d12b91f9))
* **workspace:** Skip inaccessible foreign recovery candidates ([06b2073](https://github.com/NeoTamia/NTNook/commit/06b2073163afdcf516ecdf37a283c3598164eff6))


### 🧪 Tests

* **process:** Wait for pipeline sibling to stop ([a9dc2d0](https://github.com/NeoTamia/NTNook/commit/a9dc2d08f52d2c2be4c733b1545b357806782571))
* **workspace:** Enable persistent moon fixtures in CI ([40cfd70](https://github.com/NeoTamia/NTNook/commit/40cfd7022f819d6fb295b3d8d9066667b4a770ca))
* **workspace:** Isolate moon fixture revisions from CI metadata ([c7c4bce](https://github.com/NeoTamia/NTNook/commit/c7c4bceb03ac170139b718f2fe07f3df0f6cc417))
* **workspace:** Synchronize continuous task failure before stopping peers ([3674599](https://github.com/NeoTamia/NTNook/commit/36745991d66d3b4f3684c8fd778a53dcecfab4b2))


### 🔧 Build System

* **deps:** Lock file maintenance ([#31](https://github.com/NeoTamia/NTNook/issues/31)) ([7ae6e31](https://github.com/NeoTamia/NTNook/commit/7ae6e31aec8f7c55068cb7bed1bdf9968e37c79f))
* **deps:** Lock file maintenance ([#79](https://github.com/NeoTamia/NTNook/issues/79)) ([6781837](https://github.com/NeoTamia/NTNook/commit/678183713d45100c876e0a62788f0d3fd634d7c1))
* **deps:** Update caddy:2.11.7-alpine docker digest to eb4d35b ([#74](https://github.com/NeoTamia/NTNook/issues/74)) ([0b5f116](https://github.com/NeoTamia/NTNook/commit/0b5f116957b62eec53527b82a5a6051c1f1eaf68))
* **deps:** Update pnpm to v12.14.0 ([#66](https://github.com/NeoTamia/NTNook/issues/66)) ([f7ea75d](https://github.com/NeoTamia/NTNook/commit/f7ea75da59e5128b82f099d4c11024c1f101c358))
* **deps:** Update pnpm to v12.15.0 ([#77](https://github.com/NeoTamia/NTNook/issues/77)) ([f128efc](https://github.com/NeoTamia/NTNook/commit/f128efcc8822d67321a62739d9621e700684b45d))
* **deps:** Update pnpm/setup action to v3.1.2 ([#78](https://github.com/NeoTamia/NTNook/issues/78)) ([ff4d9e1](https://github.com/NeoTamia/NTNook/commit/ff4d9e12500cc443d31561a6ff2dfd858bae05f1))
* **deps:** Update rust crate libc to v0.2.191 ([#69](https://github.com/NeoTamia/NTNook/issues/69)) ([d43f163](https://github.com/NeoTamia/NTNook/commit/d43f163073e7d422e7e4a8d4f16d7fcdafcf7503))
* **deps:** Update rust crate mdns-sd to v0.21.6 ([#70](https://github.com/NeoTamia/NTNook/issues/70)) ([aaffbbf](https://github.com/NeoTamia/NTNook/commit/aaffbbfa05e86a6715d36fc334374b0b46178a15))
* **deps:** Update rust crate ureq to v3.5.0 ([#73](https://github.com/NeoTamia/NTNook/issues/73)) ([c5b2273](https://github.com/NeoTamia/NTNook/commit/c5b22737928867cb47bafcfef86072958500099c))
* **deps:** Update rust crate uuid to v1.29.0 ([#75](https://github.com/NeoTamia/NTNook/issues/75)) ([cf8a4f2](https://github.com/NeoTamia/NTNook/commit/cf8a4f22c486d1efd6ad99c9399a70d7c4a398f6))
* **deps:** Update rust to v1.99.1 ([#65](https://github.com/NeoTamia/NTNook/issues/65)) ([947da60](https://github.com/NeoTamia/NTNook/commit/947da60be7b7e7a8d30a3f02bcd626db988b51d1))
* **deps:** Update rust-lang/crates-io-auth-action action to v1.0.6 ([#72](https://github.com/NeoTamia/NTNook/issues/72)) ([e081877](https://github.com/NeoTamia/NTNook/commit/e08187720b328fbaa12ff20c1c27a229e210df22))

## [0.9.0](https://github.com/NeoTamia/NTNook/compare/v0.8.0...v0.9.0) (2026-10-16)


### ✨ Features

* **lan:** Expose local apps through Caddy and mDNS ([faedec9](https://github.com/NeoTamia/NTNook/commit/faedec9ce11c593e11ee984e8960619000e86500))
* **lan:** Expose local apps through Caddy and mDNS ([a026de4](https://github.com/NeoTamia/NTNook/commit/a026de45b2550ac8d351e93494e7e30c6d33c482))
* **tailscale:** Add public Funnel exposure ([3275395](https://github.com/NeoTamia/NTNook/commit/327539568fe80b79e3d9d75a05af671639ad8844))
* **tailscale:** Add public Funnel exposure ([0d28131](https://github.com/NeoTamia/NTNook/commit/0d28131f67e44266f72d89d5b9842572d0392a9e))


### 🐛 Bug Fixes

* **lan:** Preserve route ownership during replacement ([1a6ee66](https://github.com/NeoTamia/NTNook/commit/1a6ee66176d93439ed716de764e865620ea7815d))
* **tailscale:** Detect foreground Funnel exposure ([a45d570](https://github.com/NeoTamia/NTNook/commit/a45d57059f3d8242fb8557230a878638ac778b86))
* **tailscale:** Keep registrations bound to their original identity ([9801000](https://github.com/NeoTamia/NTNook/commit/98010006e80e76d2fbd5696addaa56a363284344))


### 📚 Documentation

* Include Tailscale Funnel in supported scope ([b22c124](https://github.com/NeoTamia/NTNook/commit/b22c124101409bca40579abd5321f31197f220f6))


### 🔧 Build System

* **deps:** Update actions/setup-node action to v7 ([#59](https://github.com/NeoTamia/NTNook/issues/59)) ([a268ce1](https://github.com/NeoTamia/NTNook/commit/a268ce191a5bb2249f0feaad6737516557c89711))
* **deps:** Update actions/upload-artifact action to v7.0.2 ([#52](https://github.com/NeoTamia/NTNook/issues/52)) ([f919f03](https://github.com/NeoTamia/NTNook/commit/f919f039644d72088b6f03e87a960e78e4032bd3))
* **deps:** Update caddy docker tag to v2.11.7 ([#53](https://github.com/NeoTamia/NTNook/issues/53)) ([a2a74e5](https://github.com/NeoTamia/NTNook/commit/a2a74e51edad059ddf69bd1f9974f022a89e6be1))
* **deps:** Update caddy:2.11.4-alpine docker digest to 6aeddd4 ([#48](https://github.com/NeoTamia/NTNook/issues/48)) ([7f31d89](https://github.com/NeoTamia/NTNook/commit/7f31d8929289ef8a114e78d3421c8946254a7428))
* **deps:** Update dependency ubuntu to v26 ([#60](https://github.com/NeoTamia/NTNook/issues/60)) ([9a4ba29](https://github.com/NeoTamia/NTNook/commit/9a4ba2980b82ce8b68f9b78ff077b52b97f459a4))
* **deps:** Update neotamia/actions digest to 2bd963b ([#24](https://github.com/NeoTamia/NTNook/issues/24)) ([05f20d1](https://github.com/NeoTamia/NTNook/commit/05f20d11a54dea0e0856fe05af4eda754363db03))
* **deps:** Update pnpm to v11.28.5 ([#43](https://github.com/NeoTamia/NTNook/issues/43)) ([35994fa](https://github.com/NeoTamia/NTNook/commit/35994fa2ca76b90ba7d229c63a2703b1db205475))
* **deps:** Update pnpm to v12 ([#61](https://github.com/NeoTamia/NTNook/issues/61)) ([8546d6d](https://github.com/NeoTamia/NTNook/commit/8546d6d243809eb0d5b6f5d735e70179bfd4c12f))
* **deps:** Update pnpm/setup action to v3 ([#62](https://github.com/NeoTamia/NTNook/issues/62)) ([18a421f](https://github.com/NeoTamia/NTNook/commit/18a421f57f201762a7cc7e843488266cdedc844f))
* **deps:** Update rust crate clap to v4.6.7 ([#46](https://github.com/NeoTamia/NTNook/issues/46)) ([54a02c0](https://github.com/NeoTamia/NTNook/commit/54a02c0853a57d110575813d8c9185c6537cc5d2))
* **deps:** Update rust crate clap_complete to v4.6.11 ([#47](https://github.com/NeoTamia/NTNook/issues/47)) ([b908de3](https://github.com/NeoTamia/NTNook/commit/b908de3fe4fd17cfab9dfbcc1916f88706cd61ee))
* **deps:** Update rust crate ipnet to v2.12.2 ([#37](https://github.com/NeoTamia/NTNook/issues/37)) ([67d5747](https://github.com/NeoTamia/NTNook/commit/67d574736473385901d82e0134ccd7d3a5062ca5))
* **deps:** Update rust crate libc to v0.2.190 ([#54](https://github.com/NeoTamia/NTNook/issues/54)) ([78fce9f](https://github.com/NeoTamia/NTNook/commit/78fce9faef72060a48575fc998fb2d0a81739572))
* **deps:** Update rust crate serde_json to v1.0.154 ([#55](https://github.com/NeoTamia/NTNook/issues/55)) ([8d8569f](https://github.com/NeoTamia/NTNook/commit/8d8569f1e58f1c1db7c7d18d0987dc9277f55876))
* **deps:** Update rust crate sha2 to v0.11.1 ([#56](https://github.com/NeoTamia/NTNook/issues/56)) ([b9586e6](https://github.com/NeoTamia/NTNook/commit/b9586e6779a9e28ddd0fa1a0b326d53c6e0d4f6b))
* **deps:** Update rust crate signal-hook to v0.4.5 ([#57](https://github.com/NeoTamia/NTNook/issues/57)) ([3550b91](https://github.com/NeoTamia/NTNook/commit/3550b9142774fe8d5750becfce1cd4acd400fce3))
* **deps:** Update rust crate toml to v1.1.8 ([#44](https://github.com/NeoTamia/NTNook/issues/44)) ([a703fcd](https://github.com/NeoTamia/NTNook/commit/a703fcd82ed84e998a29a07df50107ce059ddf6f))
* **deps:** Update rust crate ureq to v3.4.2 ([#38](https://github.com/NeoTamia/NTNook/issues/38)) ([c6d7479](https://github.com/NeoTamia/NTNook/commit/c6d747932284ad3ea1ea5b207f87afbe1fffe1fb))
* **deps:** Update rust crate uuid to v1.28.0 ([#58](https://github.com/NeoTamia/NTNook/issues/58)) ([e20a640](https://github.com/NeoTamia/NTNook/commit/e20a640d38c63e0e41bbe8b920d9a4c7b6874bd5))
* **deps:** Update rust to v1.99.0 ([#36](https://github.com/NeoTamia/NTNook/issues/36)) ([6728e0f](https://github.com/NeoTamia/NTNook/commit/6728e0f94918a3799f36dc96937f7492c5d3d824))

## [0.8.0](https://github.com/NeoTamia/NTNook/compare/v0.7.0...v0.8.0) (2026-10-12)


### ✨ Features

* **cli:** Add run bind address option ([fea4575](https://github.com/NeoTamia/NTNook/commit/fea45751d747a2d7473304bab754432648952e14))
* **run:** Align vite nuxt next nitro and astro ([b4657d6](https://github.com/NeoTamia/NTNook/commit/b4657d6075db30369573c81cd04d5d8178aff3d0))


### 🐛 Bug Fixes

* **run:** Align Astro host and focus framework support ([0dc6275](https://github.com/NeoTamia/NTNook/commit/0dc6275d19aaa79a8aade6a6b7afec2e9e7fb82e))
* **run:** Align only serving commands and keep npm -- ([1cc3f60](https://github.com/NeoTamia/NTNook/commit/1cc3f60bafcbacf02f92941a141b9045dd6daeb3))
* **run:** Classify servers like portless ([fc4e4b7](https://github.com/NeoTamia/NTNook/commit/fc4e4b74f91491bc9880a26a9fad7a1ee2e66e5f))
* **run:** Detect frameworks from executable positions only ([f40682f](https://github.com/NeoTamia/NTNook/commit/f40682fc881c71ef2ab86ad311f636337cddb011))
* **run:** Detect versioned packages and workspace scripts ([7b5e64b](https://github.com/NeoTamia/NTNook/commit/7b5e64b8abe6ea87b85e1f0614eb43f45bb869e5))
* **run:** Drop duplicate test attribute ([4e0a964](https://github.com/NeoTamia/NTNook/commit/4e0a964129536b947c3f19ac1d507a3c34cd4070))
* **run:** Drop npm exec -- rewriting ([c04f0d2](https://github.com/NeoTamia/NTNook/commit/c04f0d2a8b904100b98916e016a7a1825eafadeb))
* **run:** Drop the leftover npm exec -- ([50afbb1](https://github.com/NeoTamia/NTNook/commit/50afbb18a55d14cb2d3255bcb2120e6f9c799751))
* **run:** Follow portless vite root and runner flags ([ee06004](https://github.com/NeoTamia/NTNook/commit/ee060044483d06ef9415c2f08c63cbd70dcb6ad4))
* **run:** Honor npm -- and pnpm workspace-root ([620e052](https://github.com/NeoTamia/NTNook/commit/620e052999def2db5f42fa28f8c21e04e721a921))
* **run:** Ignore compound package scripts ([94874b7](https://github.com/NeoTamia/NTNook/commit/94874b7b4abe447c73fbb8081590455080ca85e2))
* **run:** Inject flags only for the forced framework binary ([67bd976](https://github.com/NeoTamia/NTNook/commit/67bd976ff0587d89fed8a13447d8aa6cd70f4669))
* **run:** Keep npm exec flags off npm itself ([dbdb55f](https://github.com/NeoTamia/NTNook/commit/dbdb55f09fb24a92d86bf0841d57b7511242a088))
* **run:** Keep pnpm yarn flags before -- ([0d52289](https://github.com/NeoTamia/NTNook/commit/0d52289072d260ca6a1f29776908e779a31be5e1))
* **run:** Keep reserved port and inherited vite hosts ([27fbadb](https://github.com/NeoTamia/NTNook/commit/27fbadb151ad94ca2f98fa48a053e96738259b7f))
* **run:** Overwrite framework --host to the nook bind ([6deed2f](https://github.com/NeoTamia/NTNook/commit/6deed2ff27d0d75d822ff29e00f8b1983c403d60))
* **run:** Pass script flags without -- for pnpm and yarn ([ac3dc58](https://github.com/NeoTamia/NTNook/commit/ac3dc58107813f1e944f18d39ec122094e0f865d))
* **run:** Preserve inherited vite allowed hosts ([321f27b](https://github.com/NeoTamia/NTNook/commit/321f27b63eac79c6937368e4c52349a56d863f95))
* **run:** Put npm exec -- before the package ([b9abb1b](https://github.com/NeoTamia/NTNook/commit/b9abb1b74a97c8a2c7d010cdad25c798fa0c3225))
* **run:** Rewrite every port and skip npm global values ([15839fb](https://github.com/NeoTamia/NTNook/commit/15839fb34c721ba8361adf6a995a1951f253e5bb))
* **run:** Rewrite port only on forwarded script args ([07362c0](https://github.com/NeoTamia/NTNook/commit/07362c00566d3d99e76c7674c385842236b7d51e))
* **run:** Skip argv flags on forced framework builds ([4660f91](https://github.com/NeoTamia/NTNook/commit/4660f912aa5f12a24b67468061b3f0797b1e8c64))
* **run:** Skip Astro --root values when detecting the server ([982f2dd](https://github.com/NeoTamia/NTNook/commit/982f2ddfb3d68452ae1d1d09e48d896dfa87076b))
* **run:** Skip non-utf8 package-script arguments ([328e54a](https://github.com/NeoTamia/NTNook/commit/328e54a750b05d0bf07a92fff552865d13ce77e5))
* **run:** Treat bare next as a dev server ([481bb4e](https://github.com/NeoTamia/NTNook/commit/481bb4e266f99701ec99522771bf18a596ea178d))


### 📚 Documentation

* **run:** Clarify nook run vs nook-run ([35d2674](https://github.com/NeoTamia/NTNook/commit/35d2674a2e782e775cc05d4904bd88d419d3e2eb))


### ♻️ Code Refactoring

* **run:** Align only the framework CLI argv ([c91ac6e](https://github.com/NeoTamia/NTNook/commit/c91ac6e6615af4510846822e597a8b7aa1b5ddaa))


### 🧪 Tests

* **run:** Lock framework injection and document it ([64350c4](https://github.com/NeoTamia/NTNook/commit/64350c405fdc589cfd56f8fb36c74232845b22bb))


### 🔧 Build System

* **deps:** Update rust crate toml to v1.1.5 ([#35](https://github.com/NeoTamia/NTNook/issues/35)) ([8a07a61](https://github.com/NeoTamia/NTNook/commit/8a07a61beb2336585c13e81c8f334cf68b397b9e))
* **deps:** Update rust crate uuid to v1.26.1 ([#22](https://github.com/NeoTamia/NTNook/issues/22)) ([6820cf4](https://github.com/NeoTamia/NTNook/commit/6820cf4da08e473ddeb4a0feea0d9c730703d5a7))

## [0.7.0](https://github.com/NeoTamia/NTNook/compare/v0.6.0...v0.7.0) (2026-09-09)


### ✨ Features

* **windows:** Add dynamic PowerShell completion ([86f3073](https://github.com/NeoTamia/NTNook/commit/86f30736de4f8fc826b14b3539213c3d4bdbe711))
* **windows:** Add native runtime and release support ([eacb9ba](https://github.com/NeoTamia/NTNook/commit/eacb9badf401f99789544fe2c3c6cfd25c9df5f0))


### 🐛 Bug Fixes

* **docker:** Preserve Windows host gateway ([ab653a2](https://github.com/NeoTamia/NTNook/commit/ab653a2045d840dcb0c741bd21bf953463d649d0))
* **nook-run:** Launch Windows batch fallbacks ([ebf8d82](https://github.com/NeoTamia/NTNook/commit/ebf8d82d7361f377026c902c61fa95bd155c90fc))
* Probe loopback for wildcard application binds ([1adc9af](https://github.com/NeoTamia/NTNook/commit/1adc9afe069bc3c23d6084f6e9690a1af87a57e6))
* **unix:** Kill orphaned child groups ([db1f9bc](https://github.com/NeoTamia/NTNook/commit/db1f9bca4be014cb9b64ebe62e4016c1be30e35b))
* **unix:** Preserve raw git root paths ([b2fb19d](https://github.com/NeoTamia/NTNook/commit/b2fb19db2517274fc7361d0ffded0cfa7f5d72ae))
* **windows:** Check machine root certificates ([920dba1](https://github.com/NeoTamia/NTNook/commit/920dba1fe9a3867f7056d65a9224c586b6abb0c7))
* **windows:** Defer installer replacement ([ce80a1b](https://github.com/NeoTamia/NTNook/commit/ce80a1bd80b5a674a463d7445b55731acfd840a5))
* **windows:** Defer percent insertion for batch shims ([a02d40a](https://github.com/NeoTamia/NTNook/commit/a02d40a6cb3445b73c80563cb1015bd6bdd6851c))
* **windows:** Defer updates until binary unlocks ([278214d](https://github.com/NeoTamia/NTNook/commit/278214dc7c75fcf61cf076b8e1951769bafb818c))
* **windows:** Force failed foreground interrupts ([43588d2](https://github.com/NeoTamia/NTNook/commit/43588d2c4001dc1197f2299be6db8e333c1ea6af))
* **windows:** Force stop without console ([78a41e6](https://github.com/NeoTamia/NTNook/commit/78a41e613a236e7371b45722beae887566da354a))
* **windows:** Harden cross-console process supervision ([0eef5bd](https://github.com/NeoTamia/NTNook/commit/0eef5bd9bc5cfb65ad07b89690d67d6601d6f11a))
* **windows:** Harden portable file and shim handling ([8adecd9](https://github.com/NeoTamia/NTNook/commit/8adecd959e578bb2028a6f16ef5a97e4f6f9dd2d))
* **windows:** Match completions case-insensitively ([e2c3618](https://github.com/NeoTamia/NTNook/commit/e2c3618fc37fc16e3a9581891694da1b7407dec1))
* **windows:** Normalize updater blocker paths ([fda8935](https://github.com/NeoTamia/NTNook/commit/fda8935b99fc47dc869fe5ff93438dc3eeb74435))
* **windows:** Persist deferred installer failures ([6eb62ae](https://github.com/NeoTamia/NTNook/commit/6eb62aee6f488073755dd11cd3b1ad4fc7d282c8))
* **windows:** Preserve graceful stop semantics ([57a4a04](https://github.com/NeoTamia/NTNook/commit/57a4a045e908e06dcdd154034f64dded13a4a8ea))
* **windows:** Preserve native child exit codes ([463d336](https://github.com/NeoTamia/NTNook/commit/463d3369ece4b606227c1b15bc1ff9e47621b6bb))
* **windows:** Preserve percent signs in batch arguments ([f1b2c50](https://github.com/NeoTamia/NTNook/commit/f1b2c50837a74e4000a99655239f14acf3042a60))
* **windows:** Quote batch shim command lines ([c38fda7](https://github.com/NeoTamia/NTNook/commit/c38fda7edca438ebff4633351c6faa219b6b28ce))
* **windows:** Resolve package command shims ([41fbfb6](https://github.com/NeoTamia/NTNook/commit/41fbfb64e3bc1efc044e022e1026952240c6fc5e))
* **windows:** Retry deferred replacements ([12dd0fb](https://github.com/NeoTamia/NTNook/commit/12dd0fbd3ec490644eb7848ccad3071392ed5566))
* **windows:** Surface deferred update failures ([f1019df](https://github.com/NeoTamia/NTNook/commit/f1019df67bdfb4d08a73269940c9c8bc3aac8001))
* **windows:** Tolerate completed updater parent ([6f37b3c](https://github.com/NeoTamia/NTNook/commit/6f37b3cbd812649b831c3e93054ada24c8503133))


### 📚 Documentation

* **windows:** Define native and Docker Desktop support ([4736db2](https://github.com/NeoTamia/NTNook/commit/4736db2b04cd19026e2c42b854b026bc23e1039b))


### 🧪 Tests

* Wait for Caddy certificate issuance ([efdb99a](https://github.com/NeoTamia/NTNook/commit/efdb99a9e07919b231262374eab7b99db058a5a2))
* **windows:** Avoid reserved HTTP ports in Caddy E2E ([da9ba06](https://github.com/NeoTamia/NTNook/commit/da9ba0638611fb4b49ae42f321a2c306e1bf5bbf))
* **windows:** Bound native Caddy E2E commands ([3932f77](https://github.com/NeoTamia/NTNook/commit/3932f77c982b43477f3e3919f12a16602ab5633f))
* **windows:** Disable implicit Caddy redirect listener ([a93b8de](https://github.com/NeoTamia/NTNook/commit/a93b8de3d29defe036f7235ce66b26ad760db7f6))
* **windows:** Exercise native Caddy over HTTPS ([ef62b69](https://github.com/NeoTamia/NTNook/commit/ef62b69328fb662a679a9aa0d8385c40d778c404))
* **windows:** Isolate platform-specific CLI coverage ([d5b4b4d](https://github.com/NeoTamia/NTNook/commit/d5b4b4da5898b38450285fde84798c456ae62828))
* **windows:** Keep global config fixture portable ([2c26c43](https://github.com/NeoTamia/NTNook/commit/2c26c430576ac6ad42a26a62398d7a162ff69cc8))
* **windows:** Keep native Caddy E2E on TCP ([a8dca66](https://github.com/NeoTamia/NTNook/commit/a8dca663b727f68154af56b9b3e203d6eafc7f74))
* **windows:** Make completion checks platform-aware ([6e796a6](https://github.com/NeoTamia/NTNook/commit/6e796a6a260b811171386c334f1dcc83bed0b302))
* **windows:** Report native Caddy startup failures ([4d56aa7](https://github.com/NeoTamia/NTNook/commit/4d56aa73b2777eb6978fa689353481a859cf321f))
* **windows:** Skip privileged Caddy trust install ([5fe3c80](https://github.com/NeoTamia/NTNook/commit/5fe3c808f6400b86eb4d44a08b50edc2d5d45259))
* **windows:** Tolerate rejected config without artifacts ([89055d6](https://github.com/NeoTamia/NTNook/commit/89055d64db9f9e7ec0a935ed12146f9c61c21196))
* **windows:** Verify batch shim arguments ([35a1049](https://github.com/NeoTamia/NTNook/commit/35a1049c8db89b0ee103e8461bd85c873b61c6e3))
* **windows:** Wait for managed lease before stop ([3d3df19](https://github.com/NeoTamia/NTNook/commit/3d3df19c21ee4043807590b75e47e1b6645a1a1c))


### 🔧 Build System

* **deps:** Update pnpm to v11.25.0 ([#34](https://github.com/NeoTamia/NTNook/issues/34)) ([373a874](https://github.com/NeoTamia/NTNook/commit/373a87436b0a3cbcdb2bdf4a583a55a2b226ffeb))
* **deps:** Update pnpm/setup action to v2.1.0 ([#28](https://github.com/NeoTamia/NTNook/issues/28)) ([d51a8ad](https://github.com/NeoTamia/NTNook/commit/d51a8ad9b8c1e7a5b73a752c108059143f748d3b))

## [0.6.0](https://github.com/NeoTamia/NTNook/compare/v0.5.0...v0.6.0) (2026-08-31)


### ✨ Features

* **cli:** Add dynamic shell completions ([dc68490](https://github.com/NeoTamia/NTNook/commit/dc6849096b054dff7f2a218269a864ff02cbebae))
* **config:** Add support for configurable `run_bind_address` setting ([ebe371a](https://github.com/NeoTamia/NTNook/commit/ebe371a7870deec697777e529f49b523dc9f8abb))
* **install:** Configure shell completions ([dd0f255](https://github.com/NeoTamia/NTNook/commit/dd0f2550ecdd91aeae2ab874d452ea7db67f0496))


### 🐛 Bug Fixes

* **completions:** Address review feedback ([09515f4](https://github.com/NeoTamia/NTNook/commit/09515f49d90859f7b7752135647d7eabec25916f))

## [0.5.0](https://github.com/NeoTamia/NTNook/compare/v0.4.1...v0.5.0) (2026-08-30)


### ✨ Features

* Add project configuration init command ([7342cb5](https://github.com/NeoTamia/NTNook/commit/7342cb5501640fc94e28a542d8efcc80d962505c))
* Introduce `@neotamia/nook-run` package for JavaScript CLI integration ([a659f0f](https://github.com/NeoTamia/NTNook/commit/a659f0f254964ca3a5461821a411f1999a3de488))


### 🐛 Bug Fixes

* **ci:** Install pnpm on PATH ([3af92bc](https://github.com/NeoTamia/NTNook/commit/3af92bcb129a239dfb076651034300fc5c4467a9))
* Preserve init child arguments ([7bee144](https://github.com/NeoTamia/NTNook/commit/7bee144ba63808485a927c57bf609a62e730bf15))


### 🔧 Build System

* **deps:** Update actions/setup-node action to v6.5.0 ([#27](https://github.com/NeoTamia/NTNook/issues/27)) ([3dfe52a](https://github.com/NeoTamia/NTNook/commit/3dfe52a1ede77441c1099bced0781c853c7a4a06))


### 👷 Continuous Integration

* Split crate and package workflows ([480fabb](https://github.com/NeoTamia/NTNook/commit/480fabbaccdac102cfdda737c57ac3c58b3e4cea))

## [0.4.1](https://github.com/NeoTamia/NTNook/compare/v0.4.0...v0.4.1) (2026-08-27)


### 📚 Documentation

* Rewrite everything in english ([a7a603c](https://github.com/NeoTamia/NTNook/commit/a7a603c72197f4a1dd623ee737f7b8b4c0d8ca9f))

## [0.4.0](https://github.com/NeoTamia/NTNook/compare/v0.3.0...v0.4.0) (2026-08-26)


### ✨ Features

* **cli:** Add GitHub self-update and launch-time update notice ([508162b](https://github.com/NeoTamia/NTNook/commit/508162ba27e90efaf18e7617797d8e06482c4157))
* **cli:** Add GitHub self-update and launch-time update notice ([d336d2b](https://github.com/NeoTamia/NTNook/commit/d336d2b732522390c6b135f0736b49c8b24185fb))
* Support local project configuration ([5054097](https://github.com/NeoTamia/NTNook/commit/5054097dbfef509469d316b3d270971e45e2717a))


### 🐛 Bug Fixes

* **caddy:** Preserve Unix admin socket permissions ([89c2ae9](https://github.com/NeoTamia/NTNook/commit/89c2ae95c6c7ae7a8f9a1fcbefe8b4a38689ac7a))
* **update:** Address remaining review follow-ups ([b5fed72](https://github.com/NeoTamia/NTNook/commit/b5fed727ebe12d7d52b4a5966c81f2727e27425c))
* **update:** Replace only installer-owned binaries ([27b6ac5](https://github.com/NeoTamia/NTNook/commit/27b6ac5f06ef01942f21add1dba8f1c0b115c8fb))
* **update:** Require HOME for .local/bin and bound archive extraction ([a0d54ed](https://github.com/NeoTamia/NTNook/commit/a0d54ed7feffdd27a54a375082037308b11a2907))


### 📚 Documentation

* **readme:** Improve guidance on Caddy group permissions and socket configuration ([8aa095b](https://github.com/NeoTamia/NTNook/commit/8aa095b5bb32e6aba67c3efa46cf4cacbe3a873e))


### 🧪 Tests

* **update:** Cover checksum rejection and passive-check timeout ([8fb214a](https://github.com/NeoTamia/NTNook/commit/8fb214ab0c165a705f1d5ef72515f45e9fec4232))


### 🔧 Build System

* **deps:** Lock file maintenance ([#20](https://github.com/NeoTamia/NTNook/issues/20)) ([fb64a70](https://github.com/NeoTamia/NTNook/commit/fb64a7020ea5ed5f95aa48b1d88a04ec2a1b1496))
* **deps:** Update rust crate toml to v1 ([a6f402c](https://github.com/NeoTamia/NTNook/commit/a6f402c51665ff663447d777fe6f92a156a77087))
* **deps:** Update rust to v1.98.0 ([#10](https://github.com/NeoTamia/NTNook/issues/10)) ([d639cc3](https://github.com/NeoTamia/NTNook/commit/d639cc39eb8d58aa8cd4d0918c23f8c75e14320c))

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
