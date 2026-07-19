# Implementation log

Chronological record of every step taken implementing `PLAN.md` — refactors, failures and their fixes, tests added, findings.

---

## Step 1 — The `-javaagent` experiment (PLAN.md step 0)

**Question:** does the distribution's instrumentation agent affect the generated `gradle-wrapper.jar`? If not, drop the flag.

### Blocker: Gradle 9.7.0 does not exist

`gradle-example/`'s scripts reference **9.7.0**, but `services.gradle.org` 404s that version. The `/versions/current` API says current is **9.6.1**.
The example scripts must come from a nightly or unreleased build. Switched the experiment to 9.6.1.

**Failure worth recording:** the first download used `curl -sSL` without `-f`, which silently wrote a 9-byte file containing the text `Not Found` and exited 0. 
The implementation must check HTTP status explicitly — a download that "succeeds" with an error body is the exact failure mode that would 
poison the known-good cache. Also noted: the distribution URL returns a **307 redirect**, so the downloader must follow redirects.

### Result

Downloaded `gradle-9.6.1-bin.zip` (141 MB), checksum verified against the published `.sha256`. Unzipped, created two stub projects with an empty `build.gradle`, and ran in each:

```
java -Xmx64m -Xms64m [-javaagent:<dist>/lib/agents/gradle-instrumentation-agent-9.6.1.jar] \
     -Dorg.gradle.appname=gradle \
     -jar <dist>/lib/gradle-gradle-cli-main-9.6.1.jar wrapper --gradle-version 9.6.1
```

Both exited 0. Both produced a `gradle-wrapper.jar` with SHA-256:

```
497c8c2a7e5031f6aa847f88104aa80a93532ec32ee17bdb8d1d2f67a194a9c7
```

The only difference between the two run logs was the first run's daemon-startup banner. No agent or instrumentation warnings appeared in the no-agent run.

**→ Dropping `-javaagent`.** It has no effect on the output, so it's one less version-dependent path.

### Three findings that change the plan

1. **Gradle publishes the wrapper jar's checksum.** `/versions/current` exposes `wrapperChecksum` / `wrapperChecksumUrl`,
and `https://services.gradle.org/distributions/gradle-<v>-wrapper.jar.sha256` fetches fine.
Our generated hash **matches it exactly**. So we can verify the generated jar against Gradle's official published checksum
rather than trusting it purely because we produced it. Big trust upgrade — adding this as a required verification step.
(The jar *itself* at `gradle-<v>-wrapper.jar` 404s, so we still have to generate it; only the checksum is published.)

2. **The `wrapper` task also writes `gradlew` and `gradlew.bat`** into the stub project, alongside the jar and properties.
Our binary *is* the project's `gradlew`. Copying the stub's output wholesale into the project would overwrite our own binary with a shell script
— self-destruction on first run. **Only ever copy `gradle-wrapper.jar` and `gradle-wrapper.properties`**, never the scripts.

3. **The generated properties file has more keys than assumed** — it includes `retries=0` and `retryBackOffMs=500` on top of the expected set.
Since we ship Gradle's own generated file verbatim this costs nothing, but it confirms we should copy the generated file rather than synthesizing one ourselves.

Also noted: `lib/` contains **both** `gradle-gradle-cli-main-9.6.1.jar` and `gradle-launcher-9.6.1.jar`, so the launcher-jar lookup must *prefer* cli-main rather than treating the two as mutually exclusive. And the run started a background Gradle daemon — the generation step should pass `--no-daemon` so a throwaway work directory doesn't leave a daemon behind.

Folded all of this back into `PLAN.md`: dropped the agent, added step 6a (verify against published wrapper checksum), added the "never copy gradlew/gradlew.bat" warning, and corrected the test version from 9.7.0 to 9.6.1.

---

## Step 2 — `Cargo.toml`

Set up the package. Two things worth recording:

**Binary renamed to `gradlew`** via `[[bin]]`. The package stays `gradle-wrapper`, but the produced binary must be `gradlew` since its 
own filename becomes `-Dorg.gradle.appname`.

**Swapped reqwest → ureq.** The plan named `reqwest`, but measuring the two trees showed its blocking API is a facade over the full async stack:

| | crates | tokio/hyper/futures |
|---|---|---|
| `reqwest` (blocking, rustls) | 254 | yes |
| `ureq` (rustls) | 89 | no |

For two GETs and a file download in a short-lived CLI that `exec`s away, the async runtime is pure overhead, and it conflicted with the stated "minimal dependency" goal.
Raised it as a decision rather than switching silently; approved. Both use rustls so there's no system-OpenSSL dependency either way.

Final deps: `anyhow`, `log`, `env_logger` (no default features — drops the regex/colour machinery),
`sha2`, `zip` (deflate only), `ureq` (rustls), and `windows-sys` gated to Windows.

---

## Step 3 — `main.rs` skeleton + `properties` module

Wrote `src/main.rs` with the top-level flow and project-root discovery, currently stopping with a `bail!` right after version detection so each subsequent step can be added and tested incrementally.

`find_project_dir()` tries two strategies: next to the resolved binary (mirroring the script's `APP_HOME`, via `current_exe().canonicalize()` which also handles the symlink chain the script walks by hand), then walking up from the CWD to cover the binary being on `PATH`.

**Extracted `src/properties.rs` immediately** rather than waiting — it's self-contained and it's where a bug does the most damage, since a misparse becomes a wrong download URL.

Implemented enough of the Java `.properties` format for what Gradle emits: `#`/`!` comments, `=`/`:`/whitespace separators, backslash escapes (`\t \n \r \f \uXXXX` and literals like `\:` `\=` `\\`), and line continuations via trailing-backslash. Continuation detection counts *trailing backslashes and checks for odd parity*, so `one\\` (escaped backslash) is correctly not treated as a continuation.

Added `validate_version()` as a **security check, not cosmetic**: the version string gets interpolated into a services.gradle.org URL and into cache file names, so it must start with a digit and contain only `[A-Za-z0-9.-]`. Combined with taking only the last `/`-segment of the URL, this blocks path traversal and query-string injection.

### Tests added (9, all passing)

Built around the **verbatim** properties file Gradle generated in step 1, so the parser is tested against real output rather than my guess at the format:

- `parses_real_gradle_output` — the `https\://` escape survives as a normal URL
- `extracts_version_from_real_output` → `9.6.1`
- `accepts_all_distribution` — `-all.zip` accepted (then downgraded to `-bin` later)
- `accepts_prerelease_versions` — `9.0.0-rc-1`, `8.0-milestone-2`
- `colon_and_whitespace_separators`, `comments_and_blank_lines_ignored`, `escapes_and_continuations`
- `rejects_unparseable_urls` — a URL with no recognisable version errors rather than guessing
- `rejects_malicious_versions` — `../`, query strings, and empty versions all rejected

```
test result: ok. 9 passed; 0 failed
```

---

## Step 4 — `downloader` module

Extracted `src/downloader.rs`: streaming download, checksum verification, and unzip.

The module docstring records **why** it is so paranoid about HTTP status — the step-1 failure where a 404 body was written to disk and reported as success. Both request helpers check `status().is_success()` explicitly rather than relying on the client's defaults.

`download()` streams in 64 KB chunks and **hashes while writing**, so a 141 MB distribution never has to fit in memory and never needs a second read pass to verify.

Tests: `hex_encodes_lowercase`, `sha256_of_known_input` (against the well-known empty-string digest), `verify_is_case_insensitive_and_catches_mismatch`. 12 passing.

---

## Step 5 — `exec_replace` module

Ported verbatim from shaft, including the cargo-util attribution comment as required. `#[cfg(unix)]` uses `CommandExt::exec()`; `#[cfg(windows)]` installs a no-op console control handler then spawns and waits.

---

## Step 6 — Java discovery, JVM opts, and the full flow

Added to `main.rs` (kept there per the plan — each piece is small):

- `find_java()` mirrors the script's branch order exactly, including reproducing its two error messages verbatim, the IBM-AIX `jre/sh/java` probe, and the Windows differences (`java.exe`, existence rather than executability, quote-stripping).
- `jvm_opts()` + `split_args()` — an xargs-style tokenizer honouring single/double quotes and backslash escapes.
- `ensure_known_good()` / `generate()` / `promote()` / `sync_into_project()` / `find_launcher_jar()`.

`promote()` copies to a temp name then `rename()`s, so a concurrent invocation can never observe a half-written jar. `generate()` deletes the work directory **only on success** — on failure it's evidence, and the path is printed.

Tests added: `split_args_basic`, `split_args_quotes_are_removed`, `split_args_escapes`, `jvm_opts_order_lets_gradle_opts_win` (asserts `GRADLE_OPTS` lands last, since later JVM options win). 16 passing.

---

## Step 7 — End-to-end testing

Built a scratch project with a properties file pinned to 9.6.1 and a **deliberately malicious** `gradle-wrapper.jar` containing plain text, then ran the real binary against it with isolated `GRADLE_WRAPPER_HOME` / `GRADLE_USER_HOME`.

| Check | Result |
|---|---|
| Cold cache | Downloaded, verified, generated, promoted, replaced the planted jar, ran Gradle 9.6.1 |
| Generated jar vs Gradle's published `wrapper.jar.sha256` | `497c8c2a…a9c7` — **exact match** |
| Warm cache | 0.37s, no download, no copy |
| `git status` after warm run | clean — the hash comparison genuinely suppresses the copy |
| Tamper (overwrite jar with garbage) | silently restored to known-good, still works |
| Work directory after success | removed (parent `work/` kept, empty) |
| Exit code passthrough (`gradlew nosuchtask`) | 1 |
| `JAVA_HOME` invalid / unset | both reproduce the script's messages, exit 1 |
| Nonexistent version (99.99.99) | fails on HTTP 404, **0 files cached**, work dir preserved |
| `-all` URL | downgraded to `-bin`; project properties replaced with Gradle's own |

---

## Step 8 — Fixes found by testing

**Logging was unusable.** `RUST_LOG=debug` applied globally, so rustls handshake and ureq wire chatter buried our six useful lines. Added `init_logging()`: a *bare* level (`RUST_LOG=debug`) is now scoped to this crate, while anything more specific (`RUST_LOG=gradlew=trace,ureq=debug`) passes through untouched. The documented `RUST_LOG=debug` has to be the useful one.

**Generation output polluted stdout — a real bug.** The `wrapper` task's output was inherited straight onto stdout, so on a cold cache `./gradlew properties | grep x` would have had Gradle's build log spliced into the piped result. Now the generation child's stdout is piped and copied to **stderr**: still visible, but it can't corrupt the actual command's output. Verified by capturing stdout and stderr separately on a cold run and grepping stdout for generation markers — 0 hits.

**Clippy:** one warning (`items after a test module`) from `mod tests` sitting mid-file; moved to the end. `cargo clippy --all-targets` and `cargo fmt` are both clean.

---

## Not implemented

**The `ulimit -n` bump is missing.** PLAN.md's "Launching Java" section calls for raising `RLIMIT_NOFILE` to the hard limit on Unix, mirroring what the shell script does. This is **not in the code** — doing it needs either a `libc` dependency or a raw syscall, and adding a dependency for it seemed worth raising rather than deciding alone.

Practical impact is probably small: the limit is inherited by the JVM, and the script only warns (never dies) when it can't set it. But builds with very many open files could behave differently under this binary than under the original `gradlew`. Left as an open decision.

**Windows is written but untested.** The `#[cfg(windows)]` paths in `exec_replace` and `find_java` compile only on Windows and have never been run — this machine is Linux. They follow the `.bat` script's logic and the shaft reference, but should be treated as unverified until someone runs them.

~~**Pre-9.0 fallback is untested.**~~ **Resolved in step 9** — the `gradle-launcher-*.jar` fallback is now exercised by every fixture from 2.0 through 8.14.3.

---

## Step 9 — Fixture tests across Gradle 2.0 → 9.x

See `TEST_PLAN.md`. Everything up to here had been tested against exactly one Gradle version (9.6.1) on one JDK, leaving every version-dependent path unexercised.

### Planning findings (before any code)

- **Gradle publishes `-wrapper.jar.sha256` only sporadically for old releases.** Present for 2.x, 3.0–3.2.1, 4.4.1+; **missing** for 3.3, 3.4.1, 3.5, 3.5.1 and 4.0. The implementation hard-failed on that fetch, so those versions were unusable.
- **Java 8 runs Gradle 2.0 through 8.14.x**, so a single JDK covers seven majors. Three JDKs (`openjdk@8`, `openjdk@21`, `openjdk@25`) span the whole 2.0→9.x range.
- jabba's unqualified versions only reach down to 17; `openjdk@` covers 8–26, so one vendor serves the whole matrix.

### Implementation changes required

1. **Graceful fallback for a missing wrapper checksum.** Added `downloader::fetch_text_optional()`, which returns `None` **only** on a 404 — every other failure is still an error, so a transient outage can't silently skip a security check. When absent, the jar is trusted because it was generated by a distribution whose zip we already verified, and a warning is printed.

2. **Dropped `--gradle-version` from the generation command.** That option only exists from Gradle ~4.8 and would break every older release. It was also redundant: we run the distribution *of that exact version*, so a bare `wrapper` task already targets it. Confirmed working on Gradle 2.0.

3. **Empty `GRADLE_WRAPPER_HOME` now treated as unset**, matching `find_java()`'s handling of empty `JAVA_HOME`. Previously it resolved to a relative path and created cache directories inside the user's project.

### The bug the fixtures caught

Gradle 2.0 failed with:

```
no main manifest attribute, in .../gradle-wrapper.jar
```

Generation succeeded (`BUILD SUCCESSFUL`) — the failure was in *running* the jar. Inspecting the manifests:

| | `Main-Class` | contains `GradleWrapperMain.class` |
|---|---|---|
| gradle-wrapper 2.0 | **absent** | yes |
| gradle-wrapper 9.6.1 | present | yes |

Old wrapper jars carry no `Main-Class`, so `java -jar` cannot work — which is exactly why every `gradlew` script before 9.x used `-classpath` with the main class named explicitly, and only the 9.x script switched to `-jar`. I had copied the modern script.

**Fix:** launch with `-classpath <jar> org.gradle.wrapper.GradleWrapperMain`. That class is present in both old and new jars, so one form covers the entire 2.0→9.x range. This would have shipped broken for every pre-9.0 project — the exact case the tool was built for.

### Harness

`e2e-test/` is a separate workspace member run as a binary (`cargo run -p e2e-test`), not `cargo test`, so `cargo test` stays fast and offline. Optional filters: `cargo run -p e2e-test -- 2.0 9.6.1`.

`JABBA_HOME` is set **once at startup** to `<root>/.jabba`, before any jabba call — if it were set per-call, one missed call site would install a JDK system-wide. JDKs are resolved with `jabba which` and installed only if missing. `.gitignore` now covers `.jabba/`, `.gradle-wrapper/` and `.gradle-test-home/`.

Each fixture plants an invalid jar, runs, and asserts: correct Gradle version in stdout, jar replaced, hash matches the published wrapper checksum (or a warning where none is published), known-good cache populated, work directory cleaned, the project's own `gradlew` script untouched, and a second run that downloads and copies nothing and leaves the files byte-identical.

### A test bug the second run caught

The full suite passed 17/17 cold, then failed 16/17 when re-run:

```
FAIL  gradle 3.5.1   no published checksum for 3.5.1, but no warning was emitted
```

Not a product bug — a **bad assertion**. The "no published checksum" warning is only printed while *generating*; on a repeat suite run 3.5.1 is already in the known-good cache, so nothing is generated and nothing warns. The assertion had silently assumed a cold cache. Fixed to require the warning only when the run actually did the generation, and verified both ways: warm run reports `cached from an earlier run`, and wiping just that version's cache entry reproduces `warned, dist-verified`.

Worth noting because it's the failure mode the idempotence check exists to find — a test that only ever runs cold can encode assumptions that quietly stop holding.

### Results

**17/17 passing**, covering every Gradle major from 2.0 to 9.x:

| Fixture | Result |
|---|---|
| 2.0, 2.14.1, 4.10.3, 5.6.4, 6.9.4, 7.6.4, 8.14.3, 9.0.0, 9.6.1 | checksum verified against Gradle's published wrapper hash |
| 3.5.1 | no published checksum — warned, trusted via the verified distribution |
| tamper, all-downgrade, bad-version, exit-code, java-home-invalid, java-home-missing, empty-wrapper-home | pass |

Cold sweep took several minutes; a fully cached re-run takes **9.5s**. Caches: `.jabba` 927MB (3 JDKs), `.gradle-test-home` 1.5GB (Gradle's own distribution cache), `.gradle-wrapper` 592KB (the known-good jars — tiny, as intended).

## Final state

- `cargo test` — **16 passed, 0 failed** (fast, offline)
- `cargo run -p e2e-test` — **17 passed, 0 failed** across Gradle 2.0 → 9.6.1
- `cargo clippy --workspace --all-targets` — clean
- `cargo fmt --all` — clean
- Release binary 2.9 MB

| File | Lines |
|---|---|
| `src/main.rs` | 553 |
| `src/properties.rs` | 306 |
| `src/downloader.rs` | 161 |
| `src/exec_replace.rs` | 65 |

`main.rs` at 553 lines (roughly a third of which is comments and tests) is at the point where the plan said to re-evaluate splitting. The natural next seams would be Java discovery + JVM opts, and the known-good cache/generation logic — but neither has earned it yet on its own, so it's left as a judgement call.
