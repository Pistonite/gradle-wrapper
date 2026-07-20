# gradle-wrapper

A `gradlew` executable that is more secure than the official wrapper scheme used by Gradle.

It is a drop-in replacement for the `gradlew`/`gradlew.bat` scripts in a Java project, except that it
is installed on your system rather than committed to the project. It verifies and caches known-good
`gradle-wrapper.jar`s on your system, and applies them to each project automatically.

## The problem

Every Gradle project contains these files:

```
/gradlew
/gradlew.bat
/gradle/wrapper/gradle-wrapper.properties
/gradle/wrapper/gradle-wrapper.jar
```

Note the `.jar` - it is a binary blob. This means that if someone commits a different version of it,
the change cannot be reviewed as a diff, which makes it the perfect place to plant malicious code.
The most famous recent attack of this type is [the xz backdoor incident](https://daily.dev/blog/xz-backdoor-the-full-story-in-one-place/),
where a malicious contributor hid a backdoor inside the XZ compression library as a binary test file.

In the Gradle world, the expected workflow for a contributor is to run `./gradlew`, which invokes one
of the wrapper scripts depending on the current platform, locates the Java runtime on the system, and
runs `gradle-wrapper.jar`. This means a compromised `gradle-wrapper.jar` will be run immediately by
anyone contributing to the project. In fact, [this has already happened in 2023](https://blog.gradle.org/wrapper-attack-report).
An attacker could also plant a malicious wrapper JAR inside a legitimate-looking repository that appears to
be a useful tool. Anyone who clones and builds it will fall victim to the payload (for example, an
info stealer).

Yet the current [official guide from Gradle](https://docs.gradle.org/current/userguide/gradle_wrapper.html) still says:

>To make the Wrapper files available to other developers and execution environments, you need to check them into version control.
>
>Wrapper files, including the JAR file, are small. Adding the JAR file to version control is expected. Some organizations do not allow
>projects to submit binary files to version control, and there is no workaround available.

"No workaround available" is not exactly true - this project is proof of that - but we will come back
to it.

At the end of that page there is a section on securing the wrapper JAR for a project, which tells you
to use CI to check its integrity. However, this does not solve the problem of a malicious repository
that deliberately ships a malicious wrapper JAR.

So what does Gradle's [guide](https://blog.gradle.org/project-integrity) say about that?

> If you do not trust the project you are building, prefer using a known good, local Gradle distribution over a wrapper.

This is STUPID. The cybersecurity standard today is [Zero Trust](https://learn.microsoft.com/en-us/security/zero-trust/zero-trust-overview):
any project I clone from a remote server - *including projects that I own* - is inherently untrusted.
That advice means I would have to copy a Gradle wrapper from a trusted source every time I work on a
different project, which defeats the purpose of committing the Gradle wrapper to the repository
entirely.

Note that `gradle-wrapper.properties` can be malicious too, as can the `gradlew` scripts, but those
are text files, which makes them much easier to audit.

## This Project

This project produces a `gradlew` executable that you install locally on your computer (on your
`PATH`) and run instead of `./gradlew`.

It performs the following checks before launching `gradle-wrapper.jar`:
- The checksum of the JAR matches the official checksum published by Gradle.
- `gradle-wrapper.properties` matches the known-good version cached locally.
  - This matters because that file contains the URL used to download Gradle, which could point
    somewhere malicious.

The only information it relies on from the project being built is a single version number, deduced
from `gradle-wrapper.properties`. This means you do not have to trust the Gradle wrapper files the
project provides.

On the first run, it downloads the matching version of Gradle from Gradle and uses it to generate
`gradle-wrapper.jar`. The generated wrapper is then cached, which speeds up subsequent runs. (I am
not sure why Gradle does not publish this JAR as part of its releases, which would make the whole
process much simpler.)

**Important: for a project in any language that has a package manager, another attack vector is
always the packages it depends on. Make sure you still audit the dependencies before building the
project.**

## Install

Method 1: `cargo-binstall`, recommended if you already have Rust and cargo-binstall installed:
```
cargo binstall gradle-wrapper-cli --git https://github.com/Pistonite/gradle-wrapper
```
This downloads the prebuilt binary from the GitHub releases.

Method 2: Manually download the binary for your platform from the GitHub releases and copy it
somewhere on your `PATH`.

Method 3: Build from source. A Rust toolchain is required:
```
cargo install gradle-wrapper-cli --git https://github.com/Pistonite/gradle-wrapper
```

By default, the local cache for known-good JARs is stored at `~/.gradle-wrapper`. This can be
configured with the `GRADLE_WRAPPER_HOME` environment variable.

## Project Setup

### Your own projects

In your own projects, you can delete all the Gradle wrapper files and add them to your `.gitignore`:

```sh
rm -f gradlew gradlew.bat gradle/wrapper/gradle-wrapper.properties gradle/wrapper/gradle-wrapper.jar
git add .
git commit -m "remove gradle wrappers"
# then add them to your .gitignore
```

Then create a single file, `gradle/wrapper/.version`, containing the version of Gradle you wish to
use:
```
echo '9.6.1' > gradle/wrapper/.version
```
If this file exists, the tool uses that version instead of parsing `gradle-wrapper.properties`.
Since everyone then uses their own trusted `gradle-wrapper.properties`, this avoids generating diffs
in that file.

### Other people's projects

When working on other people's projects, simply run `gradlew build` instead of `./gradlew build`.
If the project has scripts that invoke `./gradlew`, replace that script with your own `gradlew` tool:
```
cp $(which gradlew) .
```

It is likely that this tool will generate diffs in `gradle/wrapper` in other people's projects.
If this happens to you a lot, consider using a global gitignore file.

## Usage

Identical to the `gradlew` script it replaces. Every argument is passed through to Gradle untouched:

```sh
gradlew build
gradlew test --info
gradlew :app:assembleDebug
```

## Compatibility

Tested end-to-end against Gradle 2.0 through 9.6.1.

The latest JDK tested is JDK 25. Tested on Windows, Linux, and macOS.