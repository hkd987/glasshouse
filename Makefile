# Glasshouse build entry points. `make help` lists every target.
#
# Day to day:
#   make test lint            host unit + integration tests; lint what changed since main
#   make dev-host             run glassd on this machine (insecure dev backend)
#   make vm-up vm-wait        boot the FreeBSD dev VM (first time: freebsd-fetch freebsd-sysroot)
#   make e2e-vm               cross-compile, deploy to the VM, run the end-to-end suite
#   make test-env             first-time setup: tools, FreeBSD image, VM, deploy (see docs/dev/test-environment.md)

.DEFAULT_GOAL := help
SHELL := /bin/bash
REPO_ROOT := $(CURDIR)
FREEBSD_TARGET := x86_64-unknown-freebsd
# Binaries shipped in the OS image (see os/freebsd/dist.manifest).
SHIPPED := glassd glassctl glass-mcp glass-demo-agent glass-it-agent glass-probe
# The sign-in screen and the desktop, from the shell workspace (shell/).
SHIPPED_SHELL := glass-greeter glass-shell-fb
# Cargo profile for FreeBSD builds. `vm` rebuilds in seconds for the VM loop;
# shippable images use `release` (make image-release). See
# docs/dev/build-performance.md.
FREEBSD_PROFILE ?= vm
export FREEBSD_PROFILE
export DIST_PROFILE := $(FREEBSD_PROFILE)

.PHONY: help build check test lint lint-all fmt check-layers deny sbom timings freebsd dev-host e2e-vm \
	test-env image-release legal-base ci clean shell shell-test shell-lint shell-preview shell-vm greeter-vm \
	apps-build apps-test apps-lint apps-dev

help: ## List targets
	@grep -hE '^[a-zA-Z0-9_-]+:.*## ' $(MAKEFILE_LIST) | sort | \
		awk 'BEGIN {FS = ":.*## "}; {printf "  \033[1m%-18s\033[0m %s\n", $$1, $$2}'

build: ## Build every crate for this host (debug)
	cargo build --workspace

check: ## Type-check everything, fastest feedback (no codegen)
	cargo check --workspace --all-targets

test: ## Unit and integration tests on this host
	cargo test --workspace

lint: ## Lint what changed since main: rustfmt, clippy (warnings are errors), layering, art, shellcheck
	tools/lint.py

lint-all: ## Lint everything (CI): the same checks over the whole tree
	tools/lint.py --all

fmt: ## Format all Rust code
	cargo fmt --all

check-layers: ## Enforce crate layering and manifest conventions
	tools/check-layers.py

deny: ## Check dependency licenses and advisories (needs cargo-deny)
	cargo deny check licenses bans sources

legal-base: ## Regenerate the base system's notices and source offer from its src.txz (BR-2, BR-3)
	os/freebsd/scripts/fetch.sh src
	tools/legal/base_notices.py

# The SBOM's base section: the package inventory of the last package-list image
# build (make image-pkgbase, ADR 0022), if there is one.
BASE_INVENTORY ?= $(lastword $(sort $(wildcard build/images/glasshouse-*-desktop.packages.txt)))

sbom: ## Software bill of materials, admins and security reviews only (BR-4); never ship it
	tools/legal/notices.py $(addprefix --package ,$(SHIPPED)) $(addprefix --shell-package ,$(SHIPPED_SHELL)) \
		$(if $(BASE_INVENTORY),--base-packages $(BASE_INVENTORY)) --sbom build/sbom/glasshouse.cdx.json

timings: ## Profile a clean build; opens target/cargo-timings/cargo-timing.html
	cargo build --workspace --timings

freebsd: ## Cross-compile the shipped binaries for FreeBSD (FREEBSD_PROFILE, default vm)
	cargo build --profile $(FREEBSD_PROFILE) --target $(FREEBSD_TARGET) $(addprefix -p ,$(SHIPPED))
	cargo build $(SHELL_MANIFEST) --profile $(FREEBSD_PROFILE) --target $(FREEBSD_TARGET) $(addprefix -p ,$(SHIPPED_SHELL))

dev-host: ## Run glassd here with the insecure host sandbox (development only)
	tools/dev/host-run.sh

e2e-vm: freebsd vm-deploy ## Deploy to the FreeBSD VM and run the end-to-end suite
	tests/e2e/run-vm.sh

test-env: ## First-time setup of a full test environment (tools, image, VM, deploy)
	os/freebsd/scripts/check-host-deps.sh $(if $(filter 0,$(shell id -u)),--install)
	$(MAKE) freebsd-fetch freebsd-sysroot vm-up vm-wait freebsd vm-deploy
	@echo "Test environment ready. Try: make vm-ssh, then: su demo; glassctl agents"

image-release: ## Build a shippable image: release profile (thin LTO), baked qcow2
	@tools/legal/base_notices.py --check || [ -n "$(ALLOW_DRAFT_LEGAL)" ] || \
		{ echo "image-release: set the legal contact first, or ALLOW_DRAFT_LEGAL=1 for a test image"; exit 1; }
	$(MAKE) freebsd image FREEBSD_PROFILE=release

# The desktop shell (UI-4, ADR 0009) is its own workspace under shell/, so
# the targets above never compile the UI toolkit. See shell/README.md.
SHELL_MANIFEST := --manifest-path shell/Cargo.toml

shell: ## Build the desktop shell (separate workspace; iced with the software renderer)
	cargo build $(SHELL_MANIFEST) --workspace

shell-test: ## Desktop shell tests, including the screen snapshots
	cargo test $(SHELL_MANIFEST) --workspace

shell-lint: ## Desktop shell formatting and clippy (warnings are errors)
	cargo fmt $(SHELL_MANIFEST) --all --check
	cargo clippy $(SHELL_MANIFEST) --workspace --all-targets -- -D warnings
	cargo clippy $(SHELL_MANIFEST) --workspace --all-targets --target $(FREEBSD_TARGET) -- -D warnings

shell-preview: ## Render the shell's screens to PNG in build/shell-preview (SOCKET=... for live tiles)
	cargo run $(SHELL_MANIFEST) -p glass-shell-preview -- --out build/shell-preview $(if $(SOCKET),--socket $(SOCKET))

shell-vm: ## Show the shell on the dev VM's screen with live tiles (see it with vm.sh screendump)
	cargo build $(SHELL_MANIFEST) --target $(FREEBSD_TARGET) -p glass-shell-fb
	os/freebsd/scripts/shell-vm.sh start

greeter-vm: ## Boot-like sign-in on the dev VM: the sign-in screen, then the desktop (password demo)
	cargo build $(SHELL_MANIFEST) --target $(FREEBSD_TARGET) -p glass-shell-fb -p glass-greeter
	os/freebsd/scripts/shell-vm.sh greeter

# The first-party web apps and their SDK (ADR 0009, 0024): a TypeScript
# workspace in apps/, built with esbuild, tested in headless Chromium against
# the development host's glassd and the app bridge. See apps/README.md.
APPS_BROWSERS := /opt/pw-browsers

apps/node_modules: apps/package.json apps/package-lock.json
	cd apps && npm ci --no-audit --no-fund
	@touch apps/node_modules

apps-build: apps/node_modules ## Build the web apps into apps/<app>/dist (esbuild)
	cd apps && node build.mjs

apps-test: apps-build ## Apps: type check, unit tests, Playwright e2e (cold launch and memory budgets) on dev-host glassd
	cd apps && npm run --silent typecheck
	cd apps && npm run --silent test:unit
	cargo build -p glassd -p glassctl -p glass-app-bridge
	cd apps && PLAYWRIGHT_BROWSERS_PATH=$(APPS_BROWSERS) npx playwright test -c e2e/playwright.config.ts

apps-lint: apps/node_modules ## Apps: types, manifests (DV-3), npm licenses
	cd apps && npm run --silent lint
	tools/legal/npm_licenses.py apps

apps-dev: apps-build ## Serve the apps from make dev-host's glassd: http://mail.localhost:7300
	cargo build -p glass-app-bridge
	$${CARGO_TARGET_DIR:-target}/debug/glass-app-bridge --dev --apps apps --socket build/dev/run/apps.sock

ci: lint-all test shell-lint shell-test apps-lint apps-test ## What CI runs on every change

clean: ## Remove build outputs (keeps downloaded FreeBSD images in build/cache)
	cargo clean
	rm -rf build/dist build/dev build/images

include os/freebsd/os.mk
