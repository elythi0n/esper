BINARY  := esper
BIN     := build
DIST    := dist
MODULE  := github.com/elythi0n/esper
PKG     := ./cmd/esper
CORE    := core

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
PLATFORMS := linux/amd64 linux/arm64

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

## Build the Go control plane → ./build/esper
.PHONY: build-go
build-go:
	$(GUARD_ROOT)
	@echo "building $(BINARY) $(VERSION)..."
	@mkdir -p $(BIN)
	@go build -trimpath -ldflags='$(LDFLAGS)' -o $(BIN)/$(BINARY) $(PKG)
	@echo "  → $(BIN)/$(BINARY)"

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
	@go build -trimpath -ldflags='$(LDFLAGS)' -o $(BIN)/dist/$(BINARY) $(PKG)
	@printf '  %-20s %s\n' \
		esper          "$$(du -h $(BIN)/dist/$(BINARY) | cut -f1)" \
		esper-engine   "$$(du -h $(BIN)/dist/esper-engine | cut -f1)" \
		esper-broker   "$$(du -h $(BIN)/dist/esper-broker | cut -f1)"

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
			go build -trimpath -ldflags='$(LDFLAGS)' -o $$out $(PKG) || exit 1; \
	done
	@echo "done → $(DIST)/"

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
ci: check-isolation check-reached check-trail check-host-decides fmt-check lint test build smoke

## Everything CI runs, plus the slow checks: race, miri, fuzz, deny
.PHONY: ci-full
ci-full: ci test-race deny vet-deps miri-full fuzz

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
