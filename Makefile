BINARY  := ownview
BIN     := build
DIST    := dist
MODULE  := github.com/elythi0n/ownview
PKG     := ./cmd/ownview
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
@if [ "$$(id -u)" = "0" ] && [ -z "$(OWNVIEW_ALLOW_ROOT)" ]; then \
	echo "refusing to build as root — it leaves root-owned files that break later builds."; \
	echo ""; \
	echo "  to install system-wide, build as yourself first, then install:"; \
	echo "      make build && sudo make install-only"; \
	echo ""; \
	echo "  override (not recommended): OWNVIEW_ALLOW_ROOT=1"; \
	exit 1; \
fi
endef

.PHONY: all
all: check-isolation build test

## Verify the repository references nothing outside itself
.PHONY: check-isolation
check-isolation:
	@./scripts/check-isolation.sh

## Build both binaries → ./build
.PHONY: build
build: build-rust build-go

## Build the Go control plane → ./build/ownview
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
	@cp $(CORE)/target/release/ownview-engine $(BIN)/
	@cp $(CORE)/target/release/ownview-broker $(BIN)/
	@echo "  → $(BIN)/ownview-engine, $(BIN)/ownview-broker"

## Build size-optimised release binaries → ./build/dist
##
## For scp'ing onto a server: LTO, one codegen unit, symbols stripped. Takes
## noticeably longer than `build`, so it is not what `make` does by default.
.PHONY: build-dist
build-dist:
	$(GUARD_ROOT)
	@echo "building dist binaries $(VERSION) (lto, stripped)..."
	@cd $(CORE) && cargo build --profile dist --quiet \
		--bin ownview-engine --bin ownview-broker
	@mkdir -p $(BIN)/dist
	@cp $(CORE)/target/dist/ownview-engine $(BIN)/dist/
	@cp $(CORE)/target/dist/ownview-broker $(BIN)/dist/
	@go build -trimpath -ldflags='$(LDFLAGS)' -o $(BIN)/dist/$(BINARY) $(PKG)
	@printf '  %-20s %s\n' \
		ownview          "$$(du -h $(BIN)/dist/$(BINARY) | cut -f1)" \
		ownview-engine   "$$(du -h $(BIN)/dist/ownview-engine | cut -f1)" \
		ownview-broker   "$$(du -h $(BIN)/dist/ownview-broker | cut -f1)"

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
## ownview-crypto: interpreting Ed25519 takes ~200s against ~13s for everything
## else, and dalek's unsafe is heavily audited upstream.
##
## ownview-capture and ownview-color: forbid unsafe code, no FFI, so there is no
## undefined behavior for MIRI to find. Their tests are exhaustive sweeps over
## pixels, rectangles and colours — exactly what an interpreter is worst at.
##
## ownview-encode and ownview-decode: call into Cisco's C. MIRI cannot execute
## foreign functions at all, so every test that touches the codec fails rather
## than passing — which is not a finding about our code. The decoder's real
## defence is a fuzz target with the C instrumented, plus a sandbox.
##
## The value is in the parsers, which is why the platform capture backends must
## be their own crates rather than modules of ownview-capture: X11, PipeWire
## and DXGI bindings are where undefined behavior in *our* unsafe code will
## live, and they have to stay in this set.
##
## The full run is `miri-full`, on a nightly schedule.
.PHONY: miri
miri:
	@cd $(CORE) && cargo +nightly miri test --workspace \
		--exclude ownview-crypto --exclude ownview-capture \
		--exclude ownview-color --exclude ownview-encode \
		--exclude ownview-decode

## Memory-safety check over every crate, including the slow crypto one
##
## Still excludes the two codec crates: MIRI cannot call into C, so those tests
## cannot run under it at all — the exclusion is a fact about the interpreter
## rather than a judgement about the code.
.PHONY: miri-full
miri-full:
	@cd $(CORE) && cargo +nightly miri test --workspace \
		--exclude ownview-encode --exclude ownview-decode

## Fuzz every target (needs nightly + cargo-fuzz). FUZZ_SECONDS=600 for a long run
##
## The target list lives here and nowhere else. It used to be duplicated in the
## nightly workflow, which is how `control_frames` and `damage_coalesce` were
## added and then not deep-fuzzed by anything: the push-time smoke run had them
## and the ten-minute nightly run did not. A list in two places is a list that
## disagrees with itself.
FUZZ_SECONDS ?= 30
FUZZ_TARGETS = frame_decode messages replay control_frames damage_coalesce decode_bitstream

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
	@cd $(CORE) && cargo test -p ownview-decode --test seed_corpus --release \
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
smoke:
	@./scripts/smoke.sh

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
ci: check-isolation fmt-check lint test build smoke

## Everything CI runs, plus the slow checks: race, miri, fuzz, deny
.PHONY: ci-full
ci-full: ci test-race deny vet-deps miri-full fuzz

## Install both binaries to $(PREFIX)/bin (may need sudo)
.PHONY: install
install: build install-only

## Install already-built binaries without building (sudo-safe: never builds)
.PHONY: install-only
install-only:
	@for b in $(BINARY) ownview-engine ownview-broker; do \
		if [ ! -x $(BIN)/$$b ]; then \
			echo "no binary at $(BIN)/$$b — run 'make build' first (as your normal user)"; \
			exit 1; \
		fi; \
	done
	@for b in $(BINARY) ownview-engine ownview-broker; do \
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
	@for b in $(BINARY) ownview-engine ownview-broker; do \
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
	       $(DESTDIR)$(PREFIX)/bin/ownview-engine \
	       $(DESTDIR)$(PREFIX)/bin/ownview-broker

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
