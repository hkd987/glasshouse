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
SHIPPED := glassd glassctl glass-mcp glass-demo-agent glass-it-agent glass-hardware-agent glass-desktop-agent glass-claude-runner glass-skill-evals glass-probe glass-app-bridge
# Run in the bundler's jail, which holds nothing but its own folder: statically linked, built
# apart so the flag doesn't rebuild everything else (ADR 0049).
STATIC_SHIPPED := glass-app-try
STATIC_TARGET_DIR := $(or $(CARGO_TARGET_DIR),target)/static
# The sign-in screen and the desktop, from the shell workspace (shell/).
SHIPPED_SHELL := glass-greeter glass-shell-fb glass-compositor glass-terminal glass-activity
# Cargo profile for FreeBSD builds. `vm` rebuilds in seconds for the VM loop;
# shippable images use `release` (make image-release). See
# docs/dev/build-performance.md.
FREEBSD_PROFILE ?= vm
export FREEBSD_PROFILE
export DIST_PROFILE := $(FREEBSD_PROFILE)

.PHONY: help build check test lint lint-all fmt check-layers deny sbom timings freebsd dev-host e2e-vm \
	test-env image-release legal-base ci clean shell shell-test shell-lint shell-preview shell-vm greeter-vm \
	apps-build apps-test apps-lint apps-dev freebsd-ports \
	console-dev console-test console-lint meet-test tla

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

deny: ## Check dependency licenses and advisories, both workspaces (needs cargo-deny)
	cargo deny check licenses bans sources
	cd shell && cargo deny check licenses bans sources

legal-base: ## Regenerate the base system's notices and source offer from its src.txz (BR-2, BR-3)
	os/freebsd/scripts/fetch.sh src
	tools/legal/base_notices.py

# The SBOM's base section: the package inventory of the last package-list image
# build (make image-pkgbase, ADR 0022), if there is one.
BASE_INVENTORY ?= $(lastword $(sort $(wildcard build/images/glasshouse-*-desktop.packages.txt)))

sbom: ## Software bill of materials, admins and security reviews only (BR-4); never ship it
	tools/legal/notices.py $(addprefix --package ,$(SHIPPED) $(STATIC_SHIPPED)) $(addprefix --shell-package ,$(SHIPPED_SHELL)) \
		$(if $(BASE_INVENTORY),--base-packages $(BASE_INVENTORY)) $(addprefix --third-party ,$(wildcard os/freebsd/images/*.third-party)) \
		$(if $(wildcard build/dist/freebsd/usr/local/share/licenses),--third-party-licenses build/dist/freebsd/usr/local/share/licenses) \
		--sbom build/sbom/glasshouse.cdx.json

timings: ## Profile a clean build; opens target/cargo-timings/cargo-timing.html
	cargo build --workspace --timings

freebsd: ## Cross-compile the shipped binaries for FreeBSD (FREEBSD_PROFILE, default vm)
	cargo build --profile $(FREEBSD_PROFILE) --target $(FREEBSD_TARGET) $(addprefix -p ,$(SHIPPED))
	CARGO_TARGET_X86_64_UNKNOWN_FREEBSD_RUSTFLAGS="-C target-feature=+crt-static" \
		cargo build --profile $(FREEBSD_PROFILE) --target $(FREEBSD_TARGET) --target-dir $(STATIC_TARGET_DIR) $(addprefix -p ,$(STATIC_SHIPPED))
	cargo build $(SHELL_MANIFEST) --profile $(FREEBSD_PROFILE) --target $(FREEBSD_TARGET) $(addprefix -p ,$(SHIPPED_SHELL))

dev-host: ## Run glassd here with the insecure host sandbox (development only)
	tools/dev/host-run.sh

e2e-vm: freebsd vm-deploy ## Deploy to the FreeBSD VM and run the end-to-end suite
	tests/e2e/run-vm.sh

# The dev VM takes the packages whole, with all they depend on; images take only the files
# os/freebsd/images/desktop.third-party lists (stage-dist.sh), from the same cached packages.
DESKTOP_PORTS := libxkbcommon pixman xkeyboard-config
freebsd-ports: ## Desktop libraries from packages: into the dev VM, the sysroot and the package cache
	os/freebsd/scripts/ports.sh vm-install $(DESKTOP_PORTS)
	os/freebsd/scripts/ports.sh sysroot libxkbcommon pixman

test-env: ## First-time setup of a full test environment (tools, image, VM, deploy)
	os/freebsd/scripts/check-host-deps.sh $(if $(filter 0,$(shell id -u)),--install)
	$(MAKE) freebsd-fetch freebsd-sysroot vm-up vm-wait freebsd-ports freebsd vm-deploy
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

apps-build: apps/node_modules ## Build the web apps into apps/<app>/dist (esbuild), Meet with its speech model (ADR 0045 T2)
	@tools/fetch-speech-model.sh || echo "apps-build: no speech model (offline?): Meet is built without it and won't transcribe"
	cd apps && node build.mjs

apps-test: apps-build ## Apps: type check, unit tests, Playwright e2e (cold launch and memory budgets) on dev-host glassd
	cd apps && npm run --silent typecheck
	cd apps && npm run --silent test:unit
	cargo build -p glassd -p glassctl -p glass-app-bridge -p glass-desktop-agent
	cd apps && PLAYWRIGHT_BROWSERS_PATH=$(APPS_BROWSERS) npx playwright test -c e2e/playwright.config.ts

apps-lint: apps/node_modules ## Apps: types, manifests (DV-3), npm licenses, and no base-system name in Glasshouse Agents (BR-1)
	cd apps && npm run --silent lint
	tools/legal/npm_licenses.py apps
	@! grep -rIil freebsd apps/agents/src apps/agents/index.html crates/bin/glass-agents/src crates/bin/glass/src crates/lib/agents-api/src || \
		{ echo "apps-lint: BR-1: the files above name the base system"; exit 1; }

apps-dev: apps-build ## Serve the apps from make dev-host's glassd: http://mail.localhost:7300
	cargo build -p glass-app-bridge
	$${CARGO_TARGET_DIR:-target}/debug/glass-app-bridge --dev --apps apps --socket build/dev/run/apps.sock

# The org's admin console (ADR 0023): glass-console serves the web app in
# apps/console and the API devices sync signed policy and catalog from. It
# runs on our web servers, never on a device. See apps/README.md.
CONSOLE_BINS := -p glass-console -p glass-billing -p glassd -p glassctl -p glass-app-bridge -p glass-demo-agent -p glass-probe -p glass-mcp -p glass-sfu -p glass-it-agent -p glass-desktop-agent

console-dev: apps-build ## The admin console on http://127.0.0.1:7400 (FIXTURE=500,50 fills it); enroll make dev-host with GLASS_DEV_ENROLL=build/console/enrollment.json
	cargo build -p glass-console
	$${CARGO_TARGET_DIR:-target}/debug/glass-console --dev --state build/console --web apps/console/dist $(if $(FIXTURE),--fixture $(FIXTURE))

console-test: apps-build ## Admin console: Rust tests, then Playwright on a 500-device fixture and an enrolled dev-host glassd (page and propagation budgets)
	cargo test -p glass-console -p glass-managed -p glass-app-catalog
	cargo build $(CONSOLE_BINS)
	cargo build -p glass-app-chat --example colleague
	cargo build -p glass-app-meet --example attendee
	cd apps && PLAYWRIGHT_BROWSERS_PATH=$(APPS_BROWSERS) npx playwright test -c e2e/console.config.ts

meet-test: apps-build ## Meetings' media path (CF-21, M1): two pages with the engine's fake camera, sealed with SFrame, through glass-sfu
	cargo build -p glass-sfu
	cd apps && PLAYWRIGHT_BROWSERS_PATH=$(APPS_BROWSERS) npx playwright test -c e2e/meet.config.ts

console-lint: apps/node_modules ## Admin console: clippy, types, and no base-system name in what IT sees (BR-1)
	cargo clippy -p glass-console --all-targets -- -D warnings
	cd apps && npx tsc -p tsconfig.json
	@! grep -rIil freebsd apps/console/src apps/console/index.html crates/bin/glass-console/src || \
		{ echo "console-lint: BR-1: the files above name the base system"; exit 1; }

# Glasshouse Agents (ADR 0066): each agent on a computer of its own, the
# front (glass-agents) with the web app in apps/agents, and the glass CLI.
.PHONY: agents-dev
agents-dev: ## Glasshouse Agents here: glassd with agent computers (namespaces) and the front on http://127.0.0.1:7500 (GLASS_AGENTS_MODEL=anthropic for Claude)
	tools/dev/agents-run.sh

# AWS on this machine (deploy/local): moto for S3 and KMS, and stand-ins for
# the instance role, Marketplace metering and the load balancer's sign-in.
.PHONY: aws-local aws-local-down aws-local-test
aws-local: ## AWS on this machine for Glasshouse Agents (Docker, or processes without it); writes build/local-aws/env
	tools/dev/local-aws.sh up

aws-local-down: ## Stop AWS on this machine (moto forgets everything)
	tools/dev/local-aws.sh down

aws-local-test: aws-local apps-build ## Agent computers to S3 and back, and metering, against AWS on this machine
	cargo build -p glassd -p glass-agents -p glass -p glass-desktop-agent -p glass-e2e
	. build/local-aws/env && env -u AWS_ACCESS_KEY_ID -u AWS_SECRET_ACCESS_KEY -u AWS_SESSION_TOKEN \
		AWS_EC2_METADATA_SERVICE_ENDPOINT="$$GLASS_LOCAL_IMDS" GLASS_LOCAL_REGION="$$GLASS_LOCAL_REGION" \
		GLASS_LOCAL_S3="$$GLASS_LOCAL_S3" GLASS_LOCAL_BUCKET="$$GLASS_LOCAL_BUCKET" \
		GLASS_LOCAL_KMS_KEY="$$GLASS_LOCAL_KMS_KEY" GLASS_LOCAL_METERING="$$GLASS_LOCAL_METERING" \
		GLASS_LOCAL_PRODUCT_CODE="$$GLASS_LOCAL_PRODUCT_CODE" \
		cargo test -p glass-gateway-providers -p glass-computers --lib -- --ignored local_aws
	tests/local-aws/agents-s3.sh
	tests/local-aws/people.sh

.PHONY: aws-lint
aws-lint: ## The stack a buyer deploys on AWS (deploy/aws): cfn-lint, installed into build/tools/cfn-lint on first use
	@test -x build/tools/cfn-lint/bin/cfn-lint || { python3 -m venv build/tools/cfn-lint && \
		build/tools/cfn-lint/bin/pip install --quiet cfn-lint==1.58.0; }
	build/tools/cfn-lint/bin/cfn-lint deploy/aws/glasshouse-agents.yaml

tla: ## Model-check the TLA+ models of key workflows (specs/tla, ADR 0051) with TLC
	tools/dev/tla.sh check

ci: lint-all test tla shell-lint shell-test apps-lint apps-test console-lint console-test ## What CI runs on every change

clean: ## Remove build outputs (keeps downloaded FreeBSD images in build/cache)
	cargo clean
	rm -rf build/dist build/dev build/images

include os/freebsd/os.mk
