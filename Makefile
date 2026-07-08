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
## Excludes ownview-crypto: interpreting Ed25519 under MIRI takes ~200s against
## ~13s for everything else, and dalek's unsafe is heavily audited upstream. The
## value is in the parsers and, from Phase 3, the FFI crates — which go in this
## set, because that is where undefined behavior actually lives. The full run is
## `miri-full`, on a nightly schedule.
.PHONY: miri
miri:
	@cd $(CORE) && cargo +nightly miri test --workspace --exclude ownview-crypto

## Memory-safety check over every crate, including the slow crypto one
.PHONY: miri-full
miri-full:
	@cd $(CORE) && cargo +nightly miri test --workspace

## Fuzz every target for 30s each (needs nightly + cargo-fuzz)
.PHONY: fuzz
fuzz:
	@for t in frame_decode messages replay; do \
		echo "fuzzing $$t..."; \
		(cd $(CORE) && cargo +nightly fuzz run $$t -- -max_total_time=30) || exit 1; \
	done

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
	@awk '/^## / { doc = substr($$0, 4); next } \
	      /^[a-zA-Z0-9_-]+:/ && doc { \
	          split($$1, a, ":"); printf "  %-16s %s\n", a[1], doc; doc = "" } \
	      !/^(## |\.PHONY)/ { doc = "" }' $(MAKEFILE_LIST)
