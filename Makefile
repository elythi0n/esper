BINARY  := esper
BIN     := build
DIST    := dist
MODULE  := github.com/elythi0n/esper
PKG     := ./cmd/esper
CORE    := core
WEB     := web
# Wails needs these or it compiles a stub that refuses to run — "Wails
# applications will not build without the correct build tags", at start-up
# rather than at compile time, which is a thing to find out once.
# `webkit2_41` because 4.0 is gone from current distributions.
APP_TAGS := desktop,production,webkit2_41

# Effective version — read from the VERSION file if present, else v0.0.0.
# The Rust build script reads the same file, so the Go and Rust halves can never
# disagree about which release they are.
VERSION_FILE := VERSION
VERSION      ?= $(shell [ -f $(VERSION_FILE) ] && cat $(VERSION_FILE) || echo v0.0.0)

# SOURCE_DATE_EPOCH keeps the build reproducible when the release process sets
# it; otherwise stamp the current time.
SOURCE_DATE_EPOCH ?= $(shell date -u +%s)
BUILD_TIME        := $(shell date -u -d @$(SOURCE_DATE_EPOCH) +%Y-%m-%dT%H:%M:%SZ 2>/dev/null \
                       || date -u -r $(SOURCE_DATE_EPOCH) +%Y-%m-%dT%H:%M:%SZ)
export SOURCE_DATE_EPOCH

LDFLAGS := -s -w -X main.version=$(VERSION) -X main.buildTime=$(BUILD_TIME)
PREFIX  ?= /usr/local

# **Where the build happened must not be in what the build produced.**
# `SOURCE_DATE_EPOCH` above handles *when*; these handle *where*, and both were
# measured rather than assumed — a binary built from this checkout and one
# built from `git archive` of the same commit came out with different hashes
# until these were added. `make check-reproducible` is the measurement.
#
# Go stamps the VCS state of the tree it was built in: the revision, the commit
# time, and a module version derived from them. A release tarball has no `.git`
# and so cannot produce them, which means nobody downloading the source could
# ever reproduce a binary built from a checkout. The commit is not put back as
# an ldflag on purpose — `VERSION` and the tag identify a release, and anything
# else would be one more input a reproducer has to be told.
GOFLAGS_REPRO := -trimpath -buildvcs=false

# rustc records the directory it was invoked in, and it survives `strip` — that
# was checked, because removing the symbols looks like it should be enough and
# is not. Two prefixes: the workspace, which differs between a checkout and an
# unpacked tarball, and the registry, which differs between one person's home
# directory and another's.
CARGO_HOME ?= $(HOME)/.cargo
RUSTFLAGS  := --remap-path-prefix=$(CURDIR)/$(CORE)=/esper \
              --remap-path-prefix=$(CARGO_HOME)/registry=/cargo/registry
export RUSTFLAGS

# Every platform the Go half builds for. It is pure Go with CGO off, so all of
# these work from any of them — the Rust engine is the half that needs a cross
# toolchain, and macOS needs Apple's SDK and therefore a Mac.
PLATFORMS := linux/amd64 linux/arm64 windows/amd64 darwin/amd64 darwin/arm64

# Building as root leaves root-owned files behind that break every subsequent
# build for your own user. Nothing in a build needs root, so refuse rather than
# poison the tree. Installing to a system prefix does need it — that's
# `install-only`, which never builds.
define GUARD_ROOT
@if [ "$$(id -u)" = "0" ] && [ -z "$(ESPER_ALLOW_ROOT)" ]; then \
	echo "refusing to build as root — it leaves root-owned files that break later builds."; \
	echo ""; \
	echo "  to install system-wide, build as yourself first, then install:"; \
	echo "      make build && sudo make install-only"; \
	echo ""; \
	echo "  override (not recommended): ESPER_ALLOW_ROOT=1"; \
	exit 1; \
fi
endef

.PHONY: all
all: check-isolation check-reached check-trail build test

## Verify the repository references nothing outside itself
.PHONY: check-isolation
check-isolation:
	@./scripts/check-isolation.sh

## Run the suite under contention and name anything that did not pass every time
##
## Not part of `ci`: nine full runs is twenty minutes. This is what to run when
## a test has been *seen* to fail once — and before believing that running it
## again fixed it. `make flaky ROUNDS=5 PARALLEL=4` to lean on it harder.
ROUNDS ?= 3
PARALLEL ?= 3

.PHONY: flaky
flaky:
	@./scripts/check-flaky.sh $(ROUNDS) $(PARALLEL)

## Verify every name the security path declares is reached, or recorded as not
##
## Rust cannot find this: a `pub` item in a library crate is never dead code,
## which is where three security types hid in one week. See the script.
.PHONY: check-reached
check-reached:
	@python3 ./scripts/check-reached.py

## Verify the session trail cannot leave this machine
##
## Phase 15 asks for this in those words. A sentence in a document is not a
## verification: this enumerates every reader of the trail, requires each to
## record where what it reads goes, and refuses a network of any kind in the
## crate that owns the file or in the one place that reads it.
.PHONY: check-trail
check-trail:
	@python3 ./scripts/check-trail-stays.py

## Verify the decisions the host makes alone never cross the wire
##
## A schedule the far end could read, negotiate or supply the time for would
## not be a control, and a lock state it could assert would be a switch for
## turning the policy off. Both directions are checked: the rule does not go
## out, and nothing peer-supplied comes in.
.PHONY: check-host-decides
check-host-decides:
	@python3 ./scripts/check-host-decides.py

## Verify every setting is read, and every setting read is declared
##
## Twice a setting has been a promise this product did not keep: `audit.retain`
## with a prune that had no caller, and `session.buffer` promised in a design
## document with no setting at all. A key nothing reads is a switch that does
## nothing; a key read but not declared is a typo that takes the default for
## ever with nothing saying why.
.PHONY: check-settings
check-settings:
	@python3 ./scripts/check-settings-kept.py

## Verify no setting invites a secret, and the device store is not config
##
## A warning at startup is the wrong instrument: by the time a private key is
## in `config.toml` it has been written to a file and very likely committed.
## What can be prevented is the setting that invited it.
.PHONY: check-config
check-config:
	@python3 ./scripts/check-config-holds-no-secrets.py

## Verify Rust and Go use the same words for the same things
##
## Esper is two implementations of one product, and where both halves name
## something they name it twice. A permission Rust knows and Go does not makes
## a sound trust store read as a broken one; the other way round shows a
## device holding something nothing enforces. A fingerprint word or domain
## separator out of step renders one key as two codes people then compare.
.PHONY: check-vocabularies
check-vocabularies:
	@python3 ./scripts/check-vocabularies.py

## Verify unsafe appears only where FFI needs it, and every block says why
##
## `security-engineering.md` says `forbid(unsafe_code)` on every crate that
## does not need FFI, and an unsafe inventory in the ones that do. The
## workspace implements the first half by forbidding it everywhere, which made
## the carve-out impossible rather than deliberate — `forbid` cannot be relaxed
## at a call site. This is the second half: a crate may stop inheriting the
## workspace lints only if it is listed with a reason, must re-declare every
## other rule it gave up, and must carry one documented invariant per block.
.PHONY: check-unsafe
check-unsafe:
	@python3 ./scripts/check-unsafe-inventory.py

## Verify the Windows scan-code table is derived rather than transcribed
##
## A wrong entry types the wrong character on every Windows host, and there is
## no Windows machine here to press a key on. For the AT base set an evdev code
## *is* the scan code — `KEY_A` is 30 and set-1 `a` is 0x1E — so that half of
## the table is derived from one nobody here wrote, and XKB's own keycode files
## establish it. Skips where xkeyboard-config is not installed.
.PHONY: check-scancodes
check-scancodes:
	@python3 ./scripts/check-scancodes.py

## Verify the workspace still type-checks for Windows and macOS
##
## Both targets were made clean by hand once and then checked nowhere, which is
## the same defect as a CI matrix naming a platform that has never compiled.
## `cargo check` does not link, so no cross linker is needed — but two
## dependencies build C, and without a cross toolchain `cc-rs` hands the host
## compiler Windows defines and fails in ways that say nothing about our code.
## A stub `cc`/`ar` that create the expected file and exit 0 get past them; the
## target says so rather than pretending this is a build.
.PHONY: check-other-platforms
check-other-platforms:
	@./scripts/check-other-platforms.sh

## Check the clock against Python's own timezone handling
##
## `esper-clock` reads the machine's timezone database itself, and a parser
## checked only against bytes it wrote is checked against its own
## misunderstanding. This compares tens of thousands of instants across the
## zones with the awkward shapes. Not in `ci`, because it needs Python's
## `zoneinfo` and the database, and both are absent on some machines — it says
## so rather than failing.
.PHONY: check-clock
check-clock:
	@python3 ./scripts/check-clock.py

## Build both binaries → ./build
.PHONY: build
build: build-rust build-go

## Print the RUSTFLAGS this Makefile builds with
##
## Cargo counts `RUSTFLAGS` as part of what it is building, so running `cargo`
## by hand without these compiles everything a second time into a cache of its
## own. `export RUSTFLAGS="$$(make -s print-rustflags)"` in a shell where you
## use cargo directly, and the two share one.
.PHONY: print-rustflags
print-rustflags:
	@printf '%s\n' '$(RUSTFLAGS)'

## Build the Go control plane → ./build/esper
.PHONY: build-go
build-go:
	$(GUARD_ROOT)
	@echo "building $(BINARY) $(VERSION)..."
	@mkdir -p $(BIN)
	@go build $(GOFLAGS_REPRO) -ldflags='$(LDFLAGS)' -o $(BIN)/$(BINARY) $(PKG)
	@echo "  → $(BIN)/$(BINARY)"

## Build the desktop window → ./build/esper-app
##
## The web bundle is built first and **copied** into the command's directory,
## because `go:embed` reaches down and never sideways. A placeholder
## index.html is committed there so `go build ./...` works in a checkout
## where the bundle has never been built — it says so on screen rather than
## opening a blank window.
.PHONY: build-app
build-app:
	$(GUARD_ROOT)
	@echo "building the window $(VERSION)..."
	@cd $(WEB) && pnpm install --frozen-lockfile --silent && pnpm build >/dev/null
	@rm -rf cmd/esper-app/dist
	@cp -r $(WEB)/dist cmd/esper-app/dist
	@mkdir -p $(BIN)
	@go build $(GOFLAGS_REPRO) -tags '$(APP_TAGS)' -ldflags='$(LDFLAGS)' -o $(BIN)/esper-app ./cmd/esper-app
	@git checkout -- cmd/esper-app/dist/index.html 2>/dev/null || true
	@echo "  → $(BIN)/esper-app"

## Everything the web bundle is held to: tokens, contrast, motion, inventory
.PHONY: check-web
check-web:
	$(GUARD_ROOT)
	@cd $(WEB) && pnpm install --frozen-lockfile --silent && pnpm check

## The browser gates, against a machine that is the same on every machine
##
## Every gate here drives the real dev server, which reads the real control
## plane, which reads whoever's machine it is running on — so until this target
## existed the Devices screen was measured in whatever state the developer's own
## store happened to be in, and on a fresh machine that state is **empty**. No
## device row, no permission, no pairing word had ever been rendered under a11y,
## ink, words or overflow. The rig serves a store built for the screens: a name
## nobody would type, a name that is not Latin, every permission at once, a
## pairing already spent, and a device paired and never seen.
.PHONY: check-screens
check-screens:
	$(GUARD_ROOT)
	@scripts/gate-rig.sh sh -c 'cd $(WEB) && \
	  node scripts/check-a11y.mjs     --url http://127.0.1.1:5179/ && \
	  node scripts/check-keyboard.mjs --url http://127.0.1.1:5179/ && \
	  node scripts/check-words.mjs    --url http://127.0.1.1:5179/ && \
	  node scripts/check-overflow.mjs --url http://127.0.1.1:5179/ && \
	  node scripts/check-ink.mjs      --url http://127.0.1.1:5179/'

## Measure how long the window takes to be usable, and hold it to a second
##
## The number the Phase 11 exit criterion names. Measured rather than
## estimated: the page reports its first paint and the window turns that into
## a duration from `exec`. Two arrangements — a control plane already
## listening, and the window starting its own, which is what opening the
## application actually does. Needs `make build build-app` first.
.PHONY: check-coldstart
check-coldstart:
	@./scripts/check-coldstart.sh

## Verify the link model does what it claims: one queue, or a scheduler
##
## The difference between a FIFO and a fair queue is a claim about numbers, and
## a shaper nobody measured is an opinion. It has earned this once already: the
## first fair queue handed out *twice* the rate, because a departure assigned
## on arrival cannot let a later arrival overtake — which is the entire
## behaviour a scheduler exists for.
.PHONY: check-shaper
check-shaper:
	@python3 ./scripts/flaky-link.py --self-test

## Verify what the desktop asks for reaches the built window
##
## Reduced motion and high contrast, neither of which is a preference about
## looks: somebody who set them asked their operating system for them
## everywhere. Not the /design toggles, which prove the override — this proves
## the chain a person actually uses, `gtk-enable-animations` and
## `gtk-theme-name` → WebKitGTK → the media queries → the tokens.
##
## It has earned its place twice. Putting the reduced-motion block inside a
## layer leaves the query matching and every duration untouched; and merely
## opening the Settings screen used to write the theme default as though
## somebody had chosen it, which disabled the contrast setting for ever.
## Needs `make build-app` first.
.PHONY: check-desktop
check-desktop:
	@./scripts/check-desktop.sh

## Build the Rust engine and broker → ./build
.PHONY: build-rust
build-rust:
	$(GUARD_ROOT)
	@echo "building engine + broker $(VERSION)..."
	@cd $(CORE) && cargo build --release --quiet
	@mkdir -p $(BIN)
	@cp $(CORE)/target/release/esper-engine $(BIN)/
	@cp $(CORE)/target/release/esper-broker $(BIN)/
	@echo "  → $(BIN)/esper-engine, $(BIN)/esper-broker"

## Build size-optimised release binaries → ./build/dist
##
## For scp'ing onto a server: LTO, one codegen unit, symbols stripped. Takes
## noticeably longer than `build`, so it is not what `make` does by default.
.PHONY: build-dist
build-dist:
	$(GUARD_ROOT)
	@echo "building dist binaries $(VERSION) (lto, stripped)..."
	@cd $(CORE) && cargo build --profile dist --quiet \
		--bin esper-engine --bin esper-broker
	@mkdir -p $(BIN)/dist
	@cp $(CORE)/target/dist/esper-engine $(BIN)/dist/
	@cp $(CORE)/target/dist/esper-broker $(BIN)/dist/
	@go build $(GOFLAGS_REPRO) -ldflags='$(LDFLAGS)' -o $(BIN)/dist/$(BINARY) $(PKG)
	@printf '  %-20s %s\n' \
		esper          "$$(du -h $(BIN)/dist/$(BINARY) | cut -f1)" \
		esper-engine   "$$(du -h $(BIN)/dist/esper-engine | cut -f1)" \
		esper-broker   "$$(du -h $(BIN)/dist/esper-broker | cut -f1)"

## Build real Windows binaries → ./build/win
##
## A genuine PE, linked on this machine, not a type-check. Needs `mingw-w64`,
## which is what makes `aws-lc-sys`, `openh264-sys2` and `opusic-sys` compile
## for the target — and the reason this is a separate verb from `build` is that
## those three take a while.
##
## What it does not do is say the binaries *work*: nothing here has run them.
## `esper doctor` on a Windows machine is the first thing that would.
##
## `--release` rather than the `dist` profile, to match what `build` does. The
## engine comes out around 90 MB either way — the same as the Linux release
## build, because it is the profile's debug info and not anything about the
## platform — and 13 MB stripped, which is `build-dist`'s job.
.PHONY: build-win
build-win:
	$(GUARD_ROOT)
	@command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1 || { \
		echo "no x86_64-w64-mingw32-gcc — install mingw-w64 to cross-build for Windows"; \
		exit 1; }
	@echo "building the engine and broker for windows/amd64..."
	@cd $(CORE) && cargo build --release --quiet --target x86_64-pc-windows-gnu \
		--bin esper-engine --bin esper-broker
	@mkdir -p $(BIN)/win
	@cp $(CORE)/target/x86_64-pc-windows-gnu/release/esper-engine.exe $(BIN)/win/
	@cp $(CORE)/target/x86_64-pc-windows-gnu/release/esper-broker.exe $(BIN)/win/
	@GOOS=windows GOARCH=amd64 CGO_ENABLED=0 \
		go build $(GOFLAGS_REPRO) -ldflags='$(LDFLAGS)' -o $(BIN)/win/$(BINARY).exe $(PKG)
	@printf '  %-22s %s\n' \
		esper.exe        "$$(du -h $(BIN)/win/$(BINARY).exe | cut -f1)" \
		esper-engine.exe "$$(du -h $(BIN)/win/esper-engine.exe | cut -f1)" \
		esper-broker.exe "$$(du -h $(BIN)/win/esper-broker.exe | cut -f1)"
	@echo "  → $(BIN)/win/  (a build, not a test: nothing has run these)"

## Build the Go binary for every platform → ./dist
.PHONY: build-all
build-all:
	$(GUARD_ROOT)
	@mkdir -p $(DIST)
	@for p in $(PLATFORMS); do \
		os=$${p%/*}; arch=$${p#*/}; \
		out=$(DIST)/$(BINARY)-$$os-$$arch; \
		echo "building $$out..."; \
		GOOS=$$os GOARCH=$$arch CGO_ENABLED=0 \
			go build $(GOFLAGS_REPRO) -ldflags='$(LDFLAGS)' -o $$out $(PKG) || exit 1; \
	done
	@echo "done → $(DIST)/"

## Build the Linux packages → ./dist
##
## Only the formats this machine can actually produce, which is the `.deb`s —
## the Arch packaging is a PKGBUILD for `makepkg` to drive and the RPM spec is
## for `rpmbuild`, and neither of those is a thing to reimplement here. See
## `packaging/`.
.PHONY: package-deb
package-deb: build
	$(GUARD_ROOT)
	@mkdir -p $(DIST)
	@./scripts/build-deb.sh esper $(DIST) $(BIN)
	@./scripts/build-deb.sh esper-broker $(DIST) $(BIN)

## Verify the changelog says something about the version being released
##
## Only bites once `VERSION` names a real release: while it is v0.0.0 there is
## nothing to have written about.
.PHONY: check-changelog
check-changelog:
	@./scripts/check-changelog.sh

## Build the control plane for every platform it claims, and check what came out
##
## `PLATFORMS` was a list nothing asserted. Building is cheap with CGO off, so
## this builds all five and reads each file back — a `GOARCH` the toolchain
## ignored would otherwise compile perfectly.
.PHONY: check-cross
check-cross:
	@./scripts/check-cross.sh

## Build the Arch package(s) with makepkg → ./dist
##
## A thin wrapper, deliberately. The PKGBUILD is the packaging — `makepkg` is
## what an Arch user already knows how to drive, and a Make target that
## reimplemented any of it would be a second answer to "what does the package
## contain". This only puts the result where the other formats put theirs.
.PHONY: pkg
pkg:
	$(GUARD_ROOT)
	@command -v makepkg >/dev/null || { echo "makepkg is not installed — this target is for Arch"; exit 1; }
	@mkdir -p dist
	@cd packaging/arch && makepkg -f --noconfirm
	@mv packaging/arch/*.pkg.tar.* dist/ 2>/dev/null || true
	@echo "  → dist/"
	@ls -1 dist/*.pkg.tar.* 2>/dev/null || true

## Produce the release source tarball → ./dist
##
## Tracked files only, reproducibly. This is what a stranger downloads and what
## a distribution builds from — see `check-published`.
.PHONY: dist-source
dist-source:
	@mkdir -p $(DIST)
	@./scripts/dist-source.sh $(DIST)

## Verify the artifact a stranger downloads, not the tree it came from
##
## The isolation rule is enforced against the working copy, and the working
## copy is not what is published. This unpacks the tarball and asks again —
## and plants a violation in it, because the branch of the isolation check that
## has no git to ask had never run.
.PHONY: check-published
check-published:
	@./scripts/check-published.sh

## Verify every built binary is in a package, in every format
##
## Three package definitions describing the same software is three chances to
## forget a file. They install from one manifest so they cannot disagree with
## each other — this asks the question that survives that, which is whether the
## manifest itself has kept up with the build.
.PHONY: check-packages
check-packages:
	@./scripts/check-packages.sh

## Verify every container image is pinned to a digest somebody reviewed
##
## A tag is a pointer this project does not hold, and a floating one is
## invisible until the day it moves. Runs anywhere; needs no containers.
.PHONY: check-images
check-images:
	@./scripts/check-images.sh

## Install the packages on the distributions they are for
##
## `check-packages` asks whether the manifest kept up with the build, on this
## machine, which is Arch. This asks whether `dpkg -i` works, on Debian, which
## this machine is not. Needs docker, and the images on first run.
.PHONY: check-install
check-install:
	@./scripts/check-install.sh

## Build the RPM the way Fedora would, in a Fedora
##
## The spec was written in Phase 16 and could not be built here for want of
## `rpmbuild`. This builds it, runs the project's own suite in `%check`, and
## installs what comes out. It compiles the whole workspace from source: it is
## measured in tens of minutes, not seconds, and is not part of `make all`.
.PHONY: check-rpm
check-rpm: dist-source
	@./scripts/check-rpm.sh

## Verify a build does not depend on where it happened
##
## The Go half only: seconds, and it catches the whole class. See
## `check-reproducible-full` for the engine.
.PHONY: check-reproducible
check-reproducible:
	@./scripts/check-reproducible.sh

## The same, including the engine — two cold release builds, twenty minutes
.PHONY: check-reproducible-full
check-reproducible-full:
	@./scripts/check-reproducible.sh --with-engine

## Run every test, both languages
.PHONY: test
test: test-go test-rust

.PHONY: test-go
test-go:
	@go test ./...

.PHONY: test-rust
test-rust:
	@cd $(CORE) && cargo test --workspace --quiet

## Run the Go tests with the race detector and coverage
.PHONY: test-race
test-race:
	go test -race -cover ./...

## Hold every Go package to a coverage floor, and every package to having one
##
## Not a quality target — coverage is a poor measure of whether code is tested,
## and every defect found in this tree last week was in a line already counted
## as covered. It is good at one thing, and it is the thing that happened:
## saying which code nothing has ever run. `internal/service` sat at 0.0% with
## 332 lines wrapping systemd, and nothing said so, because a package with no
## test file produces no failure. A package that appears with no floor is a
## failure here rather than a default of zero.
.PHONY: check-coverage
check-coverage:
	$(GUARD_ROOT)
	@scripts/check-coverage.sh

## Measure what the Rust tests have never run
##
## Not in `ci`: instrumented, it is a full rebuild and about fifteen minutes,
## and it is not a pass/fail gate — coverage is a poor measure of whether code
## is tested. It is good at naming code nothing has run, which is how
## `paths.rs` was found at 0.00% holding the rule that refuses an empty
## ESPER_HOME. Installs nothing: rustc's LLVM and this machine's are the same.
.PHONY: check-rust-coverage
check-rust-coverage:
	$(GUARD_ROOT)
	@scripts/check-rust-coverage.sh

## Comments that say a thing happens in one place, held to it
##
## Three faults this week were a comment asserting a property the code beneath
## it did not have — a store loader that said both halves list the same devices
## while one dropped entries, a supervisor that said it told the engine its
## name and never did, a `keep` that said it could be found afterwards and
## recorded it as something else. None was visible in a diff. Where such a
## claim is a pattern that may appear in exactly one file, this holds it there.
.PHONY: check-one-place
check-one-place:
	$(GUARD_ROOT)
	@python3 ./scripts/check-one-place.py

## One mistake, one sentence, asked of all three built binaries
##
## Every command refuses an option it does not know; what they did not do is
## agree on how to say so. Three phrasings, four shapes, and one command that
## named itself twice — none of it visible in a diff, because each line reads
## perfectly where it is written. Needs a build: the sentence is assembled from
## the parser and the dispatcher, and reading either alone cannot tell you what
## comes out.
.PHONY: check-messages
check-messages: build-rust build-go
	$(GUARD_ROOT)
	@scripts/check-messages.sh

## Every command somebody is told about can be asked about
##
## `--help` is routed to a per-command usage, and falls through to the general
## help when there is none — right for a command that takes no options, wrong
## for one that takes four. Six commands were in that state, including `grant`,
## whose usage text existed as a closure the parser used for errors and nothing
## could reach. Reads the dispatcher, the general help's own list, and the
## function each command parses its arguments with.
.PHONY: check-help
check-help:
	$(GUARD_ROOT)
	@python3 ./scripts/check-help.py

## Lint both languages at the strictest setting
.PHONY: lint
lint: lint-go lint-rust

.PHONY: lint-go
lint-go:
	@go vet ./...
	@golangci-lint run

.PHONY: lint-rust
lint-rust:
	@cd $(CORE) && cargo clippy --workspace --all-targets -- -D warnings

## Check formatting without changing anything
.PHONY: fmt-check
fmt-check:
	@test -z "$$(gofmt -s -l . | tee /dev/stderr)" || (echo "run: make fmt"; exit 1)
	@cd $(CORE) && cargo fmt --all -- --check

## Format all sources
.PHONY: fmt
fmt:
	@gofmt -s -w .
	@cd $(CORE) && cargo fmt --all

## Tidy go.mod / go.sum
.PHONY: tidy
tidy:
	go mod tidy

## Memory-safety check over the fast crates (needs nightly + miri)
##
## Two crates are excluded, for the same reason stated two ways: MIRI's cost
## here is measured and its value here is not.
##
## esper-crypto: interpreting Ed25519 takes ~200s against ~13s for everything
## else, and dalek's unsafe is heavily audited upstream.
##
## esper-capture, esper-color and esper-scale: forbid unsafe code and have
## no FFI, so there is no undefined behavior for MIRI to find. Their tests are
## exhaustive sweeps over pixels, rectangles, colours and resampling weights —
## exactly what an interpreter is worst at. esper-scale alone was 99s.
##
## esper-encode, esper-decode and esper-pipeline: call into Cisco's C,
## directly or through each other. MIRI cannot execute foreign functions at
## all, so every test that touches the codec fails rather than passing — which
## is not a finding about our code. The decoder's real defence is a fuzz target
## with the C instrumented, plus a sandbox.
##
## esper-pipeline also asks the kernel for CLOCK_PROCESS_CPUTIME_ID, which
## MIRI does not advance, so its timing tests cannot pass under it either.
##
## The value is in the parsers, which is why the platform capture backends must
## be their own crates rather than modules of esper-capture: X11, PipeWire
## and DXGI bindings are where undefined behavior in *our* unsafe code will
## live, and they have to stay in this set.
##
## The full run is `miri-full`, on a nightly schedule.
.PHONY: miri
miri:
	@cd $(CORE) && cargo +nightly miri test --workspace \
		--exclude esper-crypto --exclude esper-capture \
		--exclude esper-color --exclude esper-scale \
		--exclude esper-encode --exclude esper-decode \
		--exclude esper-pipeline

## Memory-safety check over every crate, including the slow crypto one
##
## Still excludes the two codec crates: MIRI cannot call into C, so those tests
## cannot run under it at all — the exclusion is a fact about the interpreter
## rather than a judgement about the code.
.PHONY: miri-full
miri-full:
	@cd $(CORE) && cargo +nightly miri test --workspace \
		--exclude esper-encode --exclude esper-decode \
		--exclude esper-pipeline

## Fuzz every target (needs nightly + cargo-fuzz). FUZZ_SECONDS=600 for a long run
##
## The target list lives here and nowhere else. It used to be duplicated in the
## nightly workflow, which is how `control_frames` and `damage_coalesce` were
## added and then not deep-fuzzed by anything: the push-time smoke run had them
## and the ten-minute nightly run did not. A list in two places is a list that
## disagrees with itself.
FUZZ_SECONDS ?= 30
FUZZ_TARGETS = frame_decode messages replay control_frames damage_coalesce decode_bitstream media_packets mdns_packets rendezvous_meet forward_allow

# `cargo fuzz` sets RUSTFLAGS and **not** CFLAGS, so a C dependency is compiled
# with neither sanitiser nor coverage instrumentation unless we say so. For the
# decoder target that is the difference between fuzzing and pretending to:
#
#   No ASan on the C   -> a heap overflow inside openh264 is triggered and not
#                         reported. The fuzzer finds the input and says nothing.
#   No coverage on the C -> libFuzzer gets no feedback from inside the decoder,
#                         so every mutation looks equally good and it never
#                         learns to build a valid parameter set. That is the
#                         gate every interesting path is behind: blind fuzzing,
#                         a million executions that never clear the first
#                         header check.
#
# Set for every target, not just the decoder. The pure-Rust ones have no C to
# instrument, so it costs them nothing, and a per-target exception is a thing
# someone would eventually get wrong.
# **clang, not the default cc.** These flags need it:
#
#   gcc rejects -fsanitize-coverage=inline-8bit-counters and pc-table outright
#   ("valid arguments are: trace-cmp trace-pc"), and rejects the whole option
#   when handed an assembly file — which aws-lc-sys, also in this dependency
#   graph, is full of. clang accepts all of it, including on assembly.
FUZZ_CC = clang
FUZZ_CFLAGS = -fsanitize=address -fno-omit-frame-pointer \
	-fsanitize-coverage=inline-8bit-counters,pc-table,trace-cmp

## Generate a seed corpus for the decoder from the encoder's own output
##
## A fuzzer starting from random bytes spends its budget learning that H.264
## begins with a start code and a parameter set, and everything interesting is
## behind that gate. Real bitstreams start it inside the parser rather than
## outside. Generated rather than committed, so the seeds cannot drift out of
## step with the encoder's settings.
.PHONY: fuzz-corpus
fuzz-corpus:
	@cd $(CORE) && cargo test -p esper-decode --test seed_corpus --release \
		-- --ignored --nocapture

## Regenerate the viewer stream fixture from this build's encoder
##
## The page parses this stream in TypeScript and hands it to the browser's own
## decoder, so it is a shared fixture in the sense `testdata/fixtures/README.md`
## means: a thing two implementations read, checked on both sides. Committed
## rather than generated on the fly, because the web check has to be able to
## run without a Rust toolchain.
.PHONY: viewer-fixture
viewer-fixture:
	@cd $(CORE) && cargo test -p esper-media --test viewer_fixture \
		-- --ignored --nocapture

.PHONY: fuzz
fuzz:
	@command -v $(FUZZ_CC) >/dev/null 2>&1 || { \
		echo "fuzzing needs $(FUZZ_CC): the default cc rejects the coverage"; \
		echo "flags the C dependencies have to be built with, and without them"; \
		echo "the decoder target is blind. Install clang."; exit 1; }
	@for t in $(FUZZ_TARGETS); do \
		echo "fuzzing $$t for $(FUZZ_SECONDS)s..."; \
		(cd $(CORE) && CC="$(FUZZ_CC)" CXX="$(FUZZ_CC)++" \
			CFLAGS="$(FUZZ_CFLAGS)" CXXFLAGS="$(FUZZ_CFLAGS)" \
			cargo +nightly fuzz run $$t -- -max_total_time=$(FUZZ_SECONDS)) || exit 1; \
	done

## List the fuzz targets, so CI never keeps its own copy of the list
.PHONY: fuzz-targets
fuzz-targets:
	@echo $(FUZZ_TARGETS)

## Post-build smoke test: binaries agree and ship no endpoint
.PHONY: smoke
## Depends on `build`, because it did not and a stale `build/` made a smoke
## check pass for the wrong reason: `--backend x11` was rejected as an unknown
## flag by a binary predating the flag, which reads exactly like the backend
## correctly refusing to open.
smoke: build
	@./scripts/smoke.sh

## Every performance target this project states, measured on this machine
##
## Reports what was measured against what was promised, and — when a baseline
## for this machine exists — fails if anything is worse than it. A target that
## is not met is reported and does not fail: some are not met yet, and a suite
## that goes red for a known gap is a suite people stop reading.
##
## `make bench-baseline` records what this machine did, deliberately. Never
## automatic: a baseline that ratchets on a noisy measurement records the
## luckiest run anybody ever had and then fails for everybody else.
.PHONY: bench
bench:
	@cd $(CORE) && cargo run --release --quiet -p esper-bench -- --check $(CURDIR)/bench/baseline.json

## Record what this machine measures, as a baseline to gate against
.PHONY: bench-baseline
bench-baseline:
	@cd $(CORE) && cargo run --release --quiet -p esper-bench -- --json $(CURDIR)/bench/baseline.json

## Supply-chain audit: every dependency audited or explicitly exempted
.PHONY: vet-deps
vet-deps:
	@cd $(CORE) && cargo vet check

## License and advisory audit of the Rust dependency tree
.PHONY: deny
deny:
	@cd $(CORE) && cargo deny check

## Everything CI runs, in one command
.PHONY: ci
ci: check-isolation check-reached check-shaper check-trail check-host-decides check-settings check-config check-vocabularies check-scancodes check-help check-one-place check-messages check-images check-unsafe fmt-check lint test check-coverage check-web build check-packages check-published check-cross check-changelog check-reproducible smoke

## Everything CI runs, plus the slow checks: race, miri, fuzz, deny
.PHONY: ci-full
# `check-install` and `check-rpm` are here and not in `ci`: they need docker,
# and `check-rpm` compiles the whole workspace in a container, which is tens of
# minutes rather than seconds.
ci-full: ci check-other-platforms test-race deny vet-deps miri-full fuzz check-reproducible-full check-screens check-install check-rpm

## Install both binaries to $(PREFIX)/bin (may need sudo)
.PHONY: install
install: build install-only

## Install already-built binaries without building (sudo-safe: never builds)
.PHONY: install-only
install-only:
	@for b in $(BINARY) esper-engine esper-broker; do \
		if [ ! -x $(BIN)/$$b ]; then \
			echo "no binary at $(BIN)/$$b — run 'make build' first (as your normal user)"; \
			exit 1; \
		fi; \
	done
	@for b in $(BINARY) esper-engine esper-broker; do \
		install -Dm755 $(BIN)/$$b $(DESTDIR)$(PREFIX)/bin/$$b || { \
			echo "permission denied writing to $(DESTDIR)$(PREFIX)/bin"; \
			echo "  → run : make build && sudo make install-only"; \
			echo "  → or  : make install-user   (installs to ~/.local/bin, no sudo)"; \
			exit 1; }; \
	done
	@echo "installed → $(DESTDIR)$(PREFIX)/bin/"

## Install the desktop window to ~/.local (no sudo)
##
## Apart from `install-user` because the window is apart from the rest: it
## needs a webview at runtime, and a headless machine that only wants
## `esper` and the engine should not be made to carry one. Installs the
## binary and the .desktop entry, so it appears in the launcher.
.PHONY: install-app
install-app: build-app
	@install -Dm755 $(BIN)/esper-app $(HOME)/.local/bin/esper-app
	@install -Dm644 packaging/desktop/esper.desktop $(HOME)/.local/share/applications/esper.desktop
	@# **The scheme handler needs the database rebuilt to exist.** The entry
	@# declares `x-scheme-handler/esper`; until this runs, `xdg-open` on an
	@# invitation link finds nothing and the deep link silently does nothing.
	@-command -v update-desktop-database >/dev/null 2>&1 && \
		update-desktop-database $(HOME)/.local/share/applications >/dev/null 2>&1
	@echo "installed → $(HOME)/.local/bin/esper-app"
	@echo "           $(HOME)/.local/share/applications/esper.desktop"
	@echo "           esper:// links now open this window"
	@case ":$$PATH:" in *":$(HOME)/.local/bin:"*) ;; \
		*) echo ""; echo "warning: $(HOME)/.local/bin is not on \$$PATH — add it to your shell rc";; \
	esac

## Install to ~/.local/bin (no sudo)
.PHONY: install-user
install-user: build
	@for b in $(BINARY) esper-engine esper-broker; do \
		install -Dm755 $(BIN)/$$b $(HOME)/.local/bin/$$b; \
	done
	@echo "installed → $(HOME)/.local/bin/"
	@case ":$$PATH:" in *":$(HOME)/.local/bin:"*) ;; \
		*) echo ""; echo "warning: $(HOME)/.local/bin is not on \$$PATH — add it to your shell rc";; \
	esac

## Remove the installed binaries
.PHONY: uninstall
uninstall:
	@rm -f $(DESTDIR)$(PREFIX)/bin/$(BINARY) \
	       $(DESTDIR)$(PREFIX)/bin/esper-engine \
	       $(DESTDIR)$(PREFIX)/bin/esper-broker

## Remove everything Esper installed for this user, including the service
##
## **Binaries are the easy half.** What is left behind otherwise is the part
## that keeps running: a systemd user unit that starts the host at login, an
## autostart entry that opens the window, and a desktop entry that still
## claims the `esper://` scheme. An uninstall that removed only the programs
## would leave a machine that fails at every login instead of one that has
## stopped serving.
##
## **The home is not touched.** `~/.esper` holds the identity key and the
## device store: deleting them un-pairs this machine from every other one,
## silently and unrecoverably, and that is not what "uninstall" asks for. The
## path is printed so somebody can decide for themselves.
.PHONY: uninstall-user
uninstall-user:
	@echo "stopping the host, if it is on..."
	@-command -v systemctl >/dev/null 2>&1 && { \
		systemctl --user disable --now esper.service >/dev/null 2>&1; \
		rm -f $(HOME)/.config/systemd/user/esper.service; \
		systemctl --user daemon-reload >/dev/null 2>&1; \
		echo "  removed    the systemd user unit"; \
	}
	@rm -f $(HOME)/.config/autostart/esper.desktop && echo "  removed    the autostart entry"
	@rm -f $(HOME)/.local/share/applications/esper.desktop && echo "  removed    the desktop entry"
	@-command -v update-desktop-database >/dev/null 2>&1 && \
		update-desktop-database $(HOME)/.local/share/applications >/dev/null 2>&1
	@for b in $(BINARY) esper-engine esper-broker esper-app; do \
		rm -f $(HOME)/.local/bin/$$b; \
	done
	@echo "  removed    the binaries from $(HOME)/.local/bin"
	@echo ""
	@echo "  Left alone: $(HOME)/.esper"
	@echo "  It holds this machine's identity key and which devices it trusts."
	@echo "  Deleting it un-pairs this machine from every other one, and no"
	@echo "  uninstall should decide that for you:"
	@echo ""
	@echo "      rm -rf $(HOME)/.esper"

## Remove build artifacts
.PHONY: clean
clean:
	@rm -rf $(BIN) $(DIST)
	@cd $(CORE) && cargo clean --quiet

## List targets
.PHONY: help
help:
	@awk '/^## / { if (doc == "") doc = substr($$0, 4); next } \
	      /^[a-zA-Z0-9_-]+:/ && doc { \
	          split($$1, a, ":"); printf "  %-16s %s\n", a[1], doc; doc = "" } \
	      !/^(##|\.PHONY)/ { doc = "" }' $(MAKEFILE_LIST)
