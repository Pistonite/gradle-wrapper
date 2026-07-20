# gradle-wrapper

A `gradlew` executable that is more secure than the official wrapper scheme used by Gradle.

It is a drop-in replacement for the `gradlew`/`gradlew.bat` scripts in a Java project, but
as a program installed on the system and verifies and caches known-good `gradle-wrapper.jar`s
on the system and applies it to the project automatically.

## The problem

Every Gradle project contains these files:

```
/gradlew
/gradlew.bat
/gradle/wrapper/gradle-wrapper.properties
/gradle/wrapper/gradle-wrapper.jar
```

Note the `.jar` - that is a binary blob. Meaning if someone commits a different version, it will
not be diff-able in code review, which is the perfect target for someone to plant their malicious code.
The most famous recent attack of this type is [the xz backdoor incident](https://daily.dev/blog/xz-backdoor-the-full-story-in-one-place/),
where a malicious contributor hide a backdoor inside the XZ compression library as a binary test file.

In the Gradle world, the expected workflow for a contributor is to run `./gradlew` which invokes
one of the wrapper scripts depending on the current platform, which locates the java runtime on the system
and runs `gradle-wrapper.jar`. This means a compromised `gradle-wrapper.jar` will immediately be ran
by contributors of a project. Actually, [this has already happened in 2023](https://blog.gradle.org/wrapper-attack-report).
A user could plant a malicious wrapper JAR inside a legitimate repo that looks like a useful tool.
Anyone who clones and builds the repo will fall victim to the virus (e.g. an info stealer).

Yet the current [official guide from Gradle](https://docs.gradle.org/current/userguide/gradle_wrapper.html) still mentions:

>To make the Wrapper files available to other developers and execution environments, you need to check them into version control.
>
>Wrapper files, including the JAR file, are small. Adding the JAR file to version control is expected. Some organizations do not allow
>projects to submit binary files to version control, and there is no workaround available.

"No workaround available" is not exactly true (this project as a proof), but we will come back to that.

At the end of that page there is a section for securing the wrapper jar for a project, which tells you
to use CI to check the integrity. However this does not solve the issue with a malicious repo that purposely
has a malicious wrapper JAR.

What's Gradle's [guide](https://blog.gradle.org/project-integrity) about that? 

> If you do not trust the project you are building, prefer using a known good, local Gradle distribution over a wrapper.

This is STUPID. The cybersecurity standard today is [Zero Trust](https://learn.microsoft.com/en-us/security/zero-trust/zero-trust-overview),
any project I clone from a remote server - *including projects that I own* - is inheritantly untrusted.
That means I have to copy a gradle-wrapper from a trusted source every time I work on a different project - defeating
the purpose of gradle wrapper being commited to the repo entirely.

Note the `gradle-wrapper.properties` can be malicious too, as well as the `gradlew` scripts, but those
files are text files which make them much easier to audit.

## This Project

This project produces a `gradlew` wrapper that you install locally on your computer (put in `PATH`)
that you will run, instead of `./gradlew`.

It does the following checks before launching `gradle-wrapper.jar`:
- The checksum of the JAR matches the official checksum published by Gradle
- `gradle-wrapper.properties` matches the known-good version cached locally.
  - This is important since this file contains the URL to download gradle, which can be malicious.

The only information it relies on is a single version number deduced from `gradle-wrapper.properties`
of the project being built, meaning you do not have to trust the gradle wrappers provided by the project.

On the first run, it will download the matching version of gradle from Gradle and use that to generate
`gradle-wrapper.jar`. This wrapper will then be cached which will speed up subsequent runs.
(I am not sure why Gradle does not publish this JAR as part of the release which will make the whole process
much simpler.) By default, the cache is stored at `~/.gradle-wrapper` and can be configured with the `GRADLE_WRAPPER_HOME`
environment variable.

**Important: another attack vector for project of any language that provides a package manager,
is always the packages they depend on. Make sure you still audit the dependencies before building the project.**

## Install

Method 1: `cargo-binstall`: recommended if you already have Rust and Cargo-binstall installed
```
cargo binstall gradle-wrapper-cli --git https://github.com/Pistonite/gradle-wrapper
```
This downloads the prebuilt binaries from GitHub release

Method 2: Manually download the binary for your platform from GitHub release and copy it to somewhere on your `PATH`

Method 3: Build from source. A Rust toolchain is required
```
cargo install gradle-wrapper-cli --git https://github.com/Pistonite/gradle-wrapper
```

## Project Setup
Now in your own project you can delete `gradlew`, `gradlew.bat`, and `gradle-wrapper.jar`, and add them
to your `.gitignore`. Keep `gradle-wrapper.properties` as that is the standard location for tools to find
the gradle version to use. Refer contributors to your project to this tool in your README so they
can work more securely with Gradle projects.

When working on other people's projects, simply run `gradlew build` instead of `./gradlew build`.
If the project has wrapper scripts that invoke `./gradlew`, replace the script with your `gradlew` tool
```
cp $(which gradlew) .
```

It's likely that this tool will generate a diff in `gradle/wrapper` in other people's project



 --------------------- TODO -------------------





This installs a binary named `gradlew`. Put it on your `PATH` — unlike the official script, it does
**not** need to live in the project directory, since it finds the project by walking up from your
current directory.

You can then delete `gradlew` and `gradlew.bat` from your projects, or leave them alone; this tool
ignores them and never overwrites them.

## Usage

Identical to the script it replaces. Every argument is passed through to Gradle untouched:

```sh
gradlew build
gradlew test --info
gradlew :app:assembleDebug
```

## Configuration

All configuration is by environment variable, since every command-line argument belongs to Gradle.

| Variable | Purpose |
|---|---|
| `GRADLE_WRAPPER_HOME` | Where known-good jars are cached. Defaults to `~/.gradle-wrapper`. |
| `JAVA_HOME` | The JDK to use. Same semantics as the official script, including the fallback to `java` on `PATH`. |
| `JAVA_OPTS`, `GRADLE_OPTS` | Extra JVM options, appended in that order after the defaults, so `GRADLE_OPTS` wins. |
| `GRADLE_VERSION` | Escape hatch: the version to use when it cannot be read from `gradle-wrapper.properties`. |
| `RUST_LOG` | Set to `debug` to see what the tool is doing. Off by default. |

The cache is laid out as:

```
$GRADLE_WRAPPER_HOME/
  known-good/
    gradle-wrapper-<version>.jar         # verified, trusted
    gradle-wrapper-<version>.properties
  work/<id>/                             # transient; removed on success
```

On failure the work directory is deliberately left behind, and its path printed, so you can inspect
what happened.

## What this does *not* protect you against

**Running a build still executes the project's build scripts.** `build.gradle`, `settings.gradle`,
any applied plugins, and everything they pull in are arbitrary code, evaluated with your privileges.
This tool removes one specific attack — the opaque committed binary that runs *before* any of that is
even read — but running `gradlew build` in a repository you do not trust is still running untrusted
code.

It also trusts `services.gradle.org` and the TLS chain to it. If Gradle's published checksums were
themselves compromised, verifying against them proves nothing.

Treat this as closing a door nobody was watching, not as making untrusted projects safe to build.

## Compatibility

Tested end-to-end against **Gradle 2.0 through 9.6.1** — every major version — on JDK 8, 21 and 25.

That range spans a real incompatibility worth knowing about: wrapper jars before Gradle 9 carry no
`Main-Class` manifest entry, so they cannot be launched with `java -jar` the way the modern script
does. This tool launches via `-classpath` with an explicit main class, which works across the whole
range.

Linux and macOS are supported. Windows code paths exist but are currently unverified.

## Development

```sh
cargo test                            # fast, offline unit tests
cargo run -p e2e-test                 # full end-to-end matrix (slow, downloads a lot)
cargo run -p e2e-test -- 2.0 9.6.1    # only these versions
```

The end-to-end suite provisions its own JDKs project-locally via
[jabba](https://github.com/Jabba-Team/jabba), caching everything under `.jabba/`, `.gradle-wrapper/`
and `.gradle-test-home/`. The first run downloads several gigabytes; later runs take seconds.

See `TEST_PLAN.md` for what the suite covers, and `LOG.md` for the implementation history.

## License

MIT — see [LICENSE](LICENSE).
