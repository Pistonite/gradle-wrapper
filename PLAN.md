# plan for `gradle-wrapper` (gradlew)

## Context

A Java project's `gradlew` script launches `gradle/wrapper/gradle-wrapper.jar` — a **binary blob committed into the repo**. All repos are untrusted by default, so running `./gradlew` means executing an opaque jar nobody has verified. Deleting it isn't an option: the whole toolchain (IDEs, CI, `./gradlew` invoked by other tools) depends on that file existing and working.

This project is a Rust binary named `gradlew` that replaces the `gradlew`/`gradlew.bat` shell scripts. It reads only the **version** out of `gradle-wrapper.properties`, downloads the official Gradle distribution from `services.gradle.org` over a URL it reconstructs itself, verifies it against Gradle's published SHA-256, and uses that distribution to **generate a fresh `gradle-wrapper.jar` from source of truth**. The generated jar is cached as "known-good", copied over the repo's jar, and then run normally.

Outcome: `./gradlew build` behaves exactly as before, but the jar that actually executes is one we produced from a checksum-verified official distribution, not one that arrived with the repo.

Example launch scripts (`gradle`, `gradlew`, and the `.bat` pair) are in `gradle-example/` for reference.

## Design decisions

- **Jar generation is kept**, not bypassed. We could exec the distribution's `gradle-gradle-cli-main` jar directly, but running the real wrapper jar preserves maximum compatibility with the existing Gradle toolchain (distribution caching in `~/.gradle/wrapper/dists`, IDE integration, etc.). We do not reimplement Gradle's distribution cache — the temporary distribution we download exists *only* to generate the jar and is deleted afterward.
- **Both `gradle-wrapper.jar` and `gradle-wrapper.properties` in the project are replaced** with our generated versions — we never trust the repo's copies of either. The properties file is read once for the version, then overwritten.
- **Copy is conditional on content hash.** SHA-256 the project's file straight off disk and compare against the known-good file; skip the copy when equal. No need to re-download the hash from the service — the files are small. A clean repo stays clean after the first run, with no spurious git diff on every invocation.
- **Always `-bin`.** Only the version is extracted from the properties file; the full URL is never trusted and is reconstructed against `services.gradle.org`. `-all` is downgraded (it differs only by bundled docs/sources). `distributionSha256Sum` in the repo file is ignored — we verify against the `.sha256` Gradle publishes next to the distribution.
- **The generated jar is verified against Gradle's published wrapper checksum**, not just trusted because we produced it. Gradle publishes `gradle-<v>-wrapper.jar.sha256`; our generated jar matches it exactly (verified on 9.6.1). This is the strongest available trust anchor.
- **Unix and Windows both supported**, via an `exec_replace` helper ported from shaft (see below).
- **HTTP status must be checked on every download.** A 404 body silently written to disk as a "distribution" is the failure mode that would poison the known-good cache; the distribution URL also 307-redirects, so redirects must be followed.

## Managed directory layout

`GRADLE_WRAPPER_HOME`, defaulting to `~/.gradle-wrapper`:

```
known-good/
  gradle-wrapper-<version>.jar         # generated, trusted
  gradle-wrapper-<version>.properties  # generated, trusted
work/<hash>/                           # transient; removed on success
  gradle-bin.zip
  gradle-bin/gradle-<version>/         # unzipped distribution
  stub-project/
    build.gradle                       # empty — minimum for Gradle to see a project
    gradle/wrapper/gradle-wrapper.jar  # the artifact we're after
```

`<hash>` = hash of (download URL, current time, project path), so concurrent invocations never collide. Promotion into `known-good/` writes to a temp name in the same directory and `rename()`s into place, so a half-written jar is never observable.

## Execution flow

1. **Locate the project.** Resolve `argv[0]` through symlinks exactly as the shell script does (`APP_HOME` = the directory containing the resolved binary). If `gradle/wrapper/gradle-wrapper.properties` isn't there, walk up from the CWD until found. Error clearly if neither works — this is the case where the binary was invoked from `PATH` outside any project.
2. **Find Java.** Mirror the script's logic precisely: if `JAVA_HOME` is set and non-empty, prefer `$JAVA_HOME/jre/sh/java` when executable (IBM JDK on AIX), else `$JAVA_HOME/bin/java`; die with the script's "JAVA_HOME is set to an invalid directory" message if not executable. Otherwise fall back to `java` on `PATH`, dying with the script's "JAVA_HOME is not set and no 'java' command could be found" message. On Windows: `java.exe`, existence check rather than executability, strip quotes from `JAVA_HOME`, no `jre/sh` probe.
3. **Read the version.** Parse `gradle-wrapper.properties`, pull `distributionUrl`, match `gradle-(<version>)-(bin|all)\.zip` against it, keep only the version.
4. **Cache hit?** If `known-good/gradle-wrapper-<version>.jar` exists, jump to step 7.
5. **Download + verify.** `GET https://services.gradle.org/distributions/gradle-<v>-bin.zip` and its sibling `.sha256`. Compare; abort loudly on mismatch. Unzip into `gradle-bin/` (yields a top-level `gradle-<v>/` directory).
6. **Generate the jar.** Create the stub project with an empty `build.gradle`, then run the distribution the same way its own `bin/gradle` script does — see "Launching Java" below — with args `wrapper --gradle-version <v> --no-daemon`, CWD set to `stub-project/`. On success, **verify the generated jar against Gradle's published wrapper checksum** (step 6a), then promote `stub-project/gradle/wrapper/gradle-wrapper.{jar,properties}` into `known-good/` and delete the work directory. On failure, leave the work directory in place for debugging and say where it is.

   The `wrapper` task also emits `gradlew` and `gradlew.bat` into the stub project. **Never copy those** — our binary *is* the project's `gradlew`, so copying the stub's output wholesale would overwrite us with a shell script. Only the jar and the properties file move.

6a. **Verify against Gradle's published checksum.** `GET https://services.gradle.org/distributions/gradle-<v>-wrapper.jar.sha256` and compare against the generated jar. Confirmed to match exactly for 9.6.1. This upgrades the trust story considerably: the jar isn't merely "one we built ourselves", it's provably the same jar Gradle publishes. Abort if it differs. (The jar itself is *not* downloadable — `gradle-<v>-wrapper.jar` 404s — so generation is still required; only the checksum is published.)
7. **Sync the project.** SHA-256 the project's `gradle-wrapper.jar` and `.properties` off disk; copy the known-good file over each one only where the hashes differ (or the file is missing). Log at debug when a copy actually happens.
8. **Exec.** Launch the project's now-trusted `gradle/wrapper/gradle-wrapper.jar` and hand it the user's arguments verbatim.

## Launching Java

Both launch sites (step 6, step 8) build the same argv shape the shell scripts do:

```
<java> <jvm opts> -Dorg.gradle.appname=<app base name> -jar <jar> <user args...>
```

JVM opts are the space-joined concatenation of `DEFAULT_JVM_OPTS` (`-Xmx64m -Xms64m`), then `$JAVA_OPTS`, then `$GRADLE_OPTS`, split with **xargs-style tokenization** — whitespace-separated, honoring single/double quotes and backslash escapes, quotes stripped. This ordering matters: later options win in the JVM, so `GRADLE_OPTS` must come last.

The distribution's `bin/gradle` script also passes `-javaagent:<dist>/lib/agents/gradle-instrumentation-agent-<v>.jar`. **We don't** — measured on 9.6.1, generation succeeds without it and produces a byte-identical jar, with no warnings (see LOG.md step 1). One less version-dependent path.

Rather than interpolating the version into jar names, **glob the distribution's `lib/`**: prefer `gradle-gradle-cli-main-*.jar` (Gradle 9.x), falling back to `gradle-launcher-*.jar` (pre-9.0). Note 9.6.1 ships *both*, so this is a preference order, not an either/or.

The generation run should pass `--no-daemon`, so a throwaway work directory doesn't leave a background daemon behind.

Neither script uses `-classpath` or names a main class — `-jar` picks the main class out of the manifest. There is no module path. Don't add either.

On Unix, also raise `RLIMIT_NOFILE` soft limit to the hard limit (the script's `ulimit -n` bump), warning rather than dying on failure, and skip it on macOS.

## Process replacement

Port `exec_replace` from https://github.com/Pistonite/shaft/blob/main/packages/shaftim/src/lib.rs — copy it whole, **including its attribution comment** pointing at cargo-util:

```
// Reference
// https://github.com/rust-lang/cargo/blob/master/crates/cargo-util/src/process_builder.rs
```

Unix is `CommandExt::exec()` (execvp — the process is replaced, so exit codes and signals pass through natively). Windows installs a no-op `SetConsoleCtrlHandler` so Ctrl-C reaches the child rather than killing us, then spawns, waits, and propagates the exit code. Failure codes: 255 execvp/spawn, 254 handler setup, 253 wait.

Use it for step 8. Step 6 needs the exit status to decide whether to promote the jar, so it uses an ordinary spawn-and-wait.

## Properties parsing

`gradle-wrapper.properties` is a **Java `.properties` file**, not INI. A naive `split('=')` breaks on the very first line that matters, because Gradle writes `distributionUrl=https\://services.gradle.org/...` — the `\:` is an escape. The parser must handle:

- `=` and `:` both as key/value separators, plus bare whitespace
- `#` and `!` as comment leaders; leading whitespace trimmed
- backslash escapes: `\:` `\=` `\ ` `\\` `\t` `\n` `\r` `\uXXXX`
- line continuations (trailing `\`)

This is small enough to hand-write, and it's the one place a parsing bug turns into a wrong download URL — worth unit tests.

## Project setup

Written in Rust, minimal dependencies. `Cargo.toml` currently has `name = "gradle-wrapper"`, edition 2024, and no dependencies. Needs:

- `[[bin]] name = "gradlew"` — the binary must be named `gradlew`, since `APP_BASE_NAME` feeds `-Dorg.gradle.appname` and Gradle prints it in help/error output.
- `anyhow` — error handling
- `reqwest` — downloads, blocking feature; no async runtime needed anywhere
- `sha2` — checksum verification
- `zip` — unzipping the distribution; unavoidable, the stdlib has no unzip
- `log` + `env_logger` — all logging at debug, disabled by default, enabled with `RUST_LOG=debug`
- `windows-sys` — gated to Windows, for `exec_replace`
- No CLI arg parsing — every argument is forwarded to Gradle untouched. Note this means we can never add our own flags later; anything user-facing must go through environment variables.

## Code structure

Everything is new — `src/main.rs` is still the cargo hello-world template.

**Write it all in `src/main.rs` first**, following the execution flow top to bottom, and split a module out only once a piece has earned it — when it has a clear boundary and its own tests. Don't pre-create empty files to match a diagram. The likely extractions, in the order they'll probably become worth doing:

- **`properties`** — the Java `.properties` parser and version extraction. Extract early; it's self-contained and it's the piece that most needs unit tests.
- **`downloader`** — URL reconstruction, download, SHA-256 verification, unzip.
- **`exec_replace`** — the cfg-split unix/windows port; separate because of the `#[cfg]` module split and the external attribution.

Java discovery, JVM opts assembly, project-root discovery, and the known-good sync are each small enough to stay in `main.rs` unless they grow. Re-evaluate when `main.rs` gets hard to read, not before.

## Verification

0. ~~Settle the `-javaagent` question~~ — **done, see LOG.md step 1.** Agent dropped.
1. `cargo build` — then work against a real project. `gradle-example/` has only the four launch scripts; it has **no `gradle/wrapper/gradle-wrapper.properties`**, so create a scratch project containing one pinned to **9.6.1** to test against. (Note: `gradle-example/`'s scripts reference 9.7.0, which does not exist on `services.gradle.org` — current release is 9.6.1. Don't use 9.7.0 as a test version.)
2. **Parser unit tests** on real properties content: `https\://` unescaping, `-all` URLs, `:` as separator, comments, a URL with no recognizable version (must error, not silently guess).
3. **Cold cache**: empty `GRADLE_WRAPPER_HOME`, run `gradlew --version` with `RUST_LOG=debug`. Confirm from the log that it downloaded, verified the checksum, generated the jar, promoted it, and copied into the project — and that the work directory was removed. Output should match what real `./gradlew --version` prints.
4. **Warm cache**: run again. No download, no copy (hashes match), noticeably faster. `git status` in the test project must be clean — this is the check that step 7's hash comparison actually works.
5. **Tamper test**: overwrite the project's `gradle-wrapper.jar` with garbage, run again — it must be silently replaced with the known-good jar and still work. This is the core security property.
6. **Checksum failure**: point at a version whose `.sha256` won't match (or stub the download) and confirm it aborts rather than proceeding.
7. **Java discovery**: unset `JAVA_HOME`, then set it to a nonexistent path — both must produce the same messages the shell script produces.
8. **Passthrough**: confirm exit codes propagate (`gradlew someTaskThatFails` → non-zero) and that Ctrl-C during a long build reaches Gradle rather than orphaning it.
9. **Pre-9.0**: test an 8.x `distributionUrl` to exercise the `gradle-launcher-*.jar` fallback and absent-javaagent path.
