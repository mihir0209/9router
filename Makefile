.PHONY: all build build-ui build-wasm benchmark-wasm test test-wasm run clean install-systemd status-systemd restart-systemd

TARGET_BIN := rust/app/target/release/9r
PORT ?= 20127

all: build

build:
	@./build.sh

build-ui:
	@./build.sh --rebuild-ui

build-wasm:
	@echo "Compiling Cloudflare Edge WebAssembly target..."
	@cargo build --manifest-path rust/edge-wasm/Cargo.toml --target wasm32-unknown-unknown --release

benchmark-wasm: build-wasm
	@node scripts/benchmark-edge-wasm.mjs

test:
	@echo "Running Rust test suite..."
	@cargo test --manifest-path rust/app/Cargo.toml

test-wasm:
	@echo "Running Edge WebAssembly test suite..."
	@cargo test --manifest-path rust/edge-wasm/Cargo.toml

run: build
	@echo "Launching 9Router on port $(PORT)..."
	@PORT=$(PORT) ./$(TARGET_BIN)

clean:
	@cargo clean --manifest-path rust/app/Cargo.toml
	@rm -rf rust/app/ui

install-systemd: build
	@mkdir -p $$HOME/.config/systemd/user
	@sed "s|__BINARY_PATH__|$(CURDIR)/$(TARGET_BIN)|g; s|__WORK_DIR__|$(CURDIR)|g" scripts/systemd/9router.service.template > $$HOME/.config/systemd/user/9router.service
	@systemctl --user daemon-reload
	@systemctl --user enable 9router.service
	@systemctl --user restart 9router.service
	@echo "✅ 9Router systemd service installed and started!"
	@systemctl --user status 9router.service --no-pager

status-systemd:
	@systemctl --user status 9router.service --no-pager

restart-systemd:
	@systemctl --user restart 9router.service
	@echo "✅ 9Router restarted via systemd."
