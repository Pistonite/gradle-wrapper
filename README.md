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
much simpler.)

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

By default, the local cache for known-good JARs is stored at `~/.gradle-wrapper`.
It can be configured with the `GRADLE_WRAPPER_HOME` environment variable.

## Project Setup

### Your own projects
Now in your own project you can delete all gradle wrapper files and add them to your `.gitignore`

```sh
rm -f gradlew gradlew.bat gradle/wrapper/gradle-wrapper.properties gradle/wrapper/gradle-wrapper.jar
git add .
git commit -m "remove gradle wrappers"
# then add them to your .gitignore
```

Then create a single file `/gradle/wrapper/.version` with the version of gradle you wish to use:
```
echo '9.6.1' > /gradle/wrapper/.version
```
If this file exists, this tool will prefer to use that version instead of parsing `gradle-wrapper.properties`.
Since everyone use their-trusted `gradle-wrapper.properties`, this avoids generating diffs in that file.

### Other people's projects
When working on other people's projects, simply run `gradlew build` instead of `./gradlew build`.
If the project has wrapper scripts that invoke `./gradlew`, replace the script with your `gradlew` tool
```
cp $(which gradlew) .
```

It's likely that this tool will generate diffs in `gradle/wrapper` in other people's project.
If this happens to you a lot, consider using a global git-ignore file.

## Usage

Identical to the `gradlew` script it replaces. Every argument is passed through to Gradle untouched:

```sh
gradlew build
gradlew test --info
gradlew :app:assembleDebug
```

## Compatibility

Tested end-to-end against Gradle 2.0 through 9.6.1

Latest JDK tested is JDK 25. Tested on Windows and Linux and MacOS.

