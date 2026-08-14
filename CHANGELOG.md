# Changelog

What changed, for somebody deciding whether to upgrade. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) — with the extra
rule that **the wire protocol has a version of its own**, because two Esper
installations at different releases still have to talk to each other. See
[`docs/compatibility.md`](docs/compatibility.md).

This file is written by hand and that is deliberate. Generating it from the
project's own task list was tried and rejected: those entries are engineering
notes written for whoever is building the thing, and a changelog is read by
somebody who wants to know whether to upgrade and what will break. Deriving one
from the other produces a document that serves neither. What is checked
mechanically is that this file has an entry for the version being released —
see `scripts/check-changelog.sh`.

## [Unreleased]

Nothing is released yet. Esper is pre-1.0 and has never been tagged; the
sections below describe the shape entries will take.

### Added
### Changed
### Deprecated
### Removed
### Fixed
### Security

Anything that changes what an attacker can reach goes here, whether or not it
was a vulnerability — and if it was, it names the advisory. A fix that was
never in a release does not get an entry: it never reached anybody.
