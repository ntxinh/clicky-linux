# clicky-linux — dev cycle entry point.
# Wrappers delegate to tools/*.sh; don't duplicate their logic here.

CARGO := cargo
PNPM  := pnpm --dir ui

.PHONY: build release test fmt fmt-fix clippy ui dev-ui run daemon diagnostics \
        install install-udev uninstall package setup sysroot sounds clean help

build: ## Debug build, all crates
	$(CARGO) build --workspace

release: ## Release build, all crates
	$(CARGO) build --release --workspace

test: ## Unit tests (clicky-core)
	$(CARGO) test --workspace

fmt: ## Check rustfmt
	$(CARGO) fmt --all -- --check

fmt-fix: ## Apply rustfmt
	$(CARGO) fmt --all

clippy: ## Lint, warnings as errors
	$(CARGO) clippy --workspace --all-targets -- -D warnings

ui: ## Install + build the settings UI (pnpm)
	$(PNPM) install
	$(PNPM) build

dev-ui: ## Vite dev server for the settings UI
	$(PNPM) dev

run: ## Run the app (engine + settings window)
	$(CARGO) run -p clicky

daemon: ## Run headless engine
	$(CARGO) run -p clicky -- --daemon

diagnostics: ## Device list, permission check, buffer sizes
	$(CARGO) run -p clicky -- --diagnostics

install: release ## User-level install (~/.local) via tools/install.sh
	sh tools/install.sh

install-udev: release ## Install + the sudo udev uaccess rule
	sh tools/install.sh --udev

uninstall: ## Remove user-level install
	sh tools/install.sh --uninstall

package: ## Build the RPM into dist/ (needs cargo-generate-rpm)
	sh tools/package.sh

setup: ## Print (or with INSTALL=1, run) system setup: dnf deps + udev rule
	bash tools/setup.sh $(if $(INSTALL),--install,)

sysroot: ## Populate .sysroot/ with -devel RPMs (no sudo, for gtk builds)
	bash tools/sysroot.sh

sounds: ## Regenerate the 10 synthesized CC0 banks under sounds/
	python3 tools/gen_sounds.py sounds

clean: ## cargo clean + UI dist
	$(CARGO) clean
	rm -rf ui/dist

help: ## Show this list
	@grep -hE '^[a-zA-Z_-]+:.*## ' $(MAKEFILE_LIST) | sort | \
		awk 'BEGIN{FS=":.*## "}{printf "  %-14s %s\n", $$1, $$2}'
