# Releasing Esper

Esper operates no infrastructure, and a release is where that claim is either
kept or quietly broken. Everything here exists so that somebody who does not
trust the project can check a release rather than believe it.

Nothing in this document is automatic. Tagging is a decision a person makes.

---

## What a release is

Four kinds of artifact, and each answers a different question.

| | What it is for |
|---|---|
| **Source tarball** | What a distribution builds from, and what somebody reproducing a binary starts with. `make dist-source`. |
| **Binaries** | For people who do not want to build. One per platform in the matrix. |
| **Packages** | `.deb`, and the `PKGBUILD` and `.spec` a distribution's own build uses. |
| **Checksums and signatures** | What turns "download this" into "download this and check it is what was published". |

## Reproducible builds, and what the claim actually covers

**The claim:** given the source tarball for a release, the same compiler
versions and the same target, anybody can produce a binary that is byte-for-byte
what was published.

It is worth being exact about why this matters here. Esper's whole position is
that no server of ours is in the path — but the binaries are, and a binary is
the one place a backdoor cannot be seen by reading the code. A reproducible
build is what lets somebody other than us check that the published binary is
the published source.

`make check-reproducible` builds the control plane twice from two different
directories and requires the bytes to match. `make check-reproducible-full`
does the same for the engine and the broker, which takes about twenty minutes
and is a release step rather than a CI step.

Two things had to be fixed for the claim to be true at all, and both were
found by measuring rather than by reading:

- **Go stamps the VCS state of the tree it was built in** — the revision, the
  commit time, a module version derived from them. A release tarball has no
  `.git`, so a binary built from the tarball could never match one built from a
  checkout. Anybody who downloaded the source to verify a release would have
  concluded it had been tampered with. `-buildvcs=false` now, everywhere.
- **rustc records the directory it was invoked in**, and it survives `strip`.
  `--remap-path-prefix` for the workspace and for the cargo registry, so
  neither where the tree sits nor whose home directory built it is in the
  output.

**What is still not covered**: a different compiler version, a different libc,
a different linker. Reproducibility is relative to a stated toolchain, and the
toolchain is stated — `rust-toolchain.toml` pins Rust, and the Go version is in
`go.mod`. Two people on the same toolchain should agree; that is the claim, and
`make check-reproducible-full` is one machine checking what one machine can.

## Signing and checksums

- `SHA256SUMS` covering every artifact, and a detached signature over that
  file rather than over each artifact. One signature to check, and the file it
  covers names everything.
- **The signing key is held offline** and is not in CI. A key a build server
  can use is a key a compromised build server can use, and the entire point of
  signing a reproducible build is to attest something the build server cannot
  fake on its own.
- The public key is published in the repository and in the release, and its
  fingerprint belongs somewhere people can check it independently.

## Update policy: Esper does not check for updates

**There is no update ping, and there will not be one.** A client that asks a
server whether it is current tells that server, on a schedule, that this
machine exists, what version it runs, and roughly where it is. That is a
census, and taking one is exactly the thing this product exists not to do.

Updates come from wherever the software came from: a package manager if it was
installed from a package, or a person checking the releases page. `esper
version` says what is installed so that comparison is possible; it does not
make it for you.

## A security fix does not wait for a feature release

If a fix has a security consequence, it ships on its own, from a branch off the
last release rather than from the development tip. Otherwise a fix waits for
whatever else is unfinished, and the pressure to ship the unfinished thing
comes from the fix.

- Patch release off the release tag, with only the fix and its test.
- An entry under `### Security` in the changelog, naming the advisory.
- The advisory says what an attacker could do, which versions are affected,
  and what to do if upgrading is not possible today.

See [`SECURITY.md`](../SECURITY.md) for how a report reaches us in the first
place.

---

## The checklist

Nothing below is run by CI. Each step is either a command whose output is read
or a decision somebody makes.

### Before the tag

- [ ] `make ci-full` is green, including `check-reproducible-full`.
- [ ] `VERSION` names the release, and `make check-changelog` agrees.
- [ ] The changelog's `[Unreleased]` section has been read and turned into a
      section for this version — in the language of somebody deciding whether
      to upgrade, not in the language of the commits.
- [ ] `make check-published` is green: the tarball is what a stranger gets, it
      builds, and it names nothing they cannot reach.
- [ ] Anything under `### Security` has an advisory written and a date agreed.
- [ ] `docs/compatibility.md` still describes what this release does to the
      wire protocol. If the protocol version changed, that section is the one
      people will read.

### Building

- [ ] `make dist-source` — the tarball.
- [ ] `make build-all` and `make build-win` — the binaries.
- [ ] `make package-deb`, and the `PKGBUILD` and `.spec` for whoever builds
      those.
- [ ] `sha256sum` over everything, into `SHA256SUMS`.
- [ ] Sign `SHA256SUMS` with the offline key.

### On real machines

The cross-compiled artifacts have been *built* and not *run*. Before a release:

- [ ] The Windows binaries start and `esper doctor` is sane, on Windows.
- [ ] The macOS binaries start and `esper doctor` is sane, on macOS.
- [ ] A Linux package installs, the service starts, and two machines connect.

### After the tag

- [ ] The published artifacts' checksums match the ones built locally.
- [ ] A second person reproduces at least one binary from the source tarball
      and gets the same hash. **This is the step that makes the claim mean
      anything**; a reproducible build nobody has independently reproduced is a
      build system property, not a guarantee to a user.

---

## What is not yet true

Written down rather than left to be discovered.

- **SLSA build provenance** is not attached to releases. It needs a build
  service whose attestations mean something, and the decision about which one
  has not been made.
- **The RPM spec has never been built.** This project is developed on Arch,
  which has no `rpmbuild`. `make check-packages` verifies its file list against
  the same manifest the other formats use and cannot verify that `rpmbuild`
  accepts the file.
- **Nothing is notarized**, and macOS packaging is not built here at all.
- **No release has ever been tagged**, so this checklist has not been run end
  to end. What *has* been run is a dry run of every step this machine can do —
  see below.

## The dry run, 2026-09-10

Every step that does not need a tag, a signing key or another operating system,
run on a development machine, with what happened written down. Two findings,
and the second is the reason to do this before a release rather than during
one.

| Step | |
|---|---|
| `make check-changelog` | ✅ |
| `make check-published` | ❌ **the tarball is built from what git tracks**, and a working tree with uncommitted work produces a tarball that does not compile. It says exactly which files are missing, which is the check doing its job. |
| `make check-packages` | ❌ then ✅ — it required *every built binary* to be in a package, and `esper-app` is built and deliberately not packaged until the viewer exists. Fixed by letting `packaging/contents.tsv` decline a binary **with a reason**, so the check holds that decision instead of forcing the opposite one. |
| `make dist-source` | ✅ |
| `make build-all` | ✅ linux amd64/arm64, darwin amd64/arm64 |
| `make build-win` | ✅ real PE binaries, 61s |
| `make package-deb` | ✅ — and it fails with `Text file busy` if an `esper-engine` is running, which is worth knowing before a release rather than during one |
| `sha256sum` over `dist/` | ✅ |

**What this cannot rehearse**, and remains exactly as untested as before:
signing (the key is offline), the three "on real machines" steps, and every
step after the tag. `make ci-full` was not part of the dry run either — it runs
miri and the fuzzers and is measured in tens of minutes.
