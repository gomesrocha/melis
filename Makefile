# Melis AI Gateway - Build & Release
# ====================================
#
# Targets:
#   make build          - Build for current platform (release)
#   make build-all      - Build for Linux, macOS, Windows (requires cross)
#   make build-linux    - Linux x86_64 (musl static)
#   make build-mac      - macOS x86_64 + ARM64 (Apple Silicon)
#   make build-windows  - Windows x86_64
#   make docker         - Build Docker image
#   make release        - Build all + package into dist/
#   make clean          - Remove build artifacts
#   make install-cross  - Install the 'cross' tool for cross-compilation
#
# Prerequisites:
#   - Rust toolchain (rustup)
#   - Docker (for cross-compilation and Docker image)
#   - cross: cargo install cross --git https://github.com/cross-rs/cross
#

VERSION := $(shell grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/')
BINARY_NAME := melis-gateway
DIST_DIR := dist

.PHONY: build build-all build-linux build-mac build-windows docker release clean install-cross

# ─── Local Build ──────────────────────────────────────────────────────────────

build:
	cargo build --release

run: build
	./target/release/$(BINARY_NAME)

# ─── Cross-Compilation ────────────────────────────────────────────────────────

install-cross:
	cargo install cross --version 0.2.5

build-linux:
	cross build --release --target x86_64-unknown-linux-musl
	@echo "✓ Linux x86_64: target/x86_64-unknown-linux-musl/release/$(BINARY_NAME)"

build-linux-arm:
	cross build --release --target aarch64-unknown-linux-musl
	@echo "✓ Linux ARM64: target/aarch64-unknown-linux-musl/release/$(BINARY_NAME)"

build-mac:
	cross build --release --target x86_64-apple-darwin
	@echo "✓ macOS x86_64: target/x86_64-apple-darwin/release/$(BINARY_NAME)"

build-mac-arm:
	cross build --release --target aarch64-apple-darwin
	@echo "✓ macOS ARM64: target/aarch64-apple-darwin/release/$(BINARY_NAME)"

build-windows:
	cross build --release --target x86_64-pc-windows-gnu
	@echo "✓ Windows x86_64: target/x86_64-pc-windows-gnu/release/$(BINARY_NAME).exe"

build-all: build-linux build-linux-arm build-mac build-mac-arm build-windows

# ─── Docker ───────────────────────────────────────────────────────────────────

docker:
	docker build -t $(BINARY_NAME):$(VERSION) -t $(BINARY_NAME):latest .

docker-push:
	docker push $(BINARY_NAME):$(VERSION)
	docker push $(BINARY_NAME):latest

# ─── Release Packaging ────────────────────────────────────────────────────────

release: build-all
	@mkdir -p $(DIST_DIR)
	@echo "📦 Packaging releases..."

	# Linux x86_64
	@mkdir -p $(DIST_DIR)/$(BINARY_NAME)-$(VERSION)-linux-x86_64
	@cp target/x86_64-unknown-linux-musl/release/$(BINARY_NAME) $(DIST_DIR)/$(BINARY_NAME)-$(VERSION)-linux-x86_64/
	@cp config.yaml.example $(DIST_DIR)/$(BINARY_NAME)-$(VERSION)-linux-x86_64/ 2>/dev/null || true
	@cp routes.yaml.example $(DIST_DIR)/$(BINARY_NAME)-$(VERSION)-linux-x86_64/ 2>/dev/null || true
	@cp README.md $(DIST_DIR)/$(BINARY_NAME)-$(VERSION)-linux-x86_64/ 2>/dev/null || true
	@cd $(DIST_DIR) && tar czf $(BINARY_NAME)-$(VERSION)-linux-x86_64.tar.gz $(BINARY_NAME)-$(VERSION)-linux-x86_64/
	@rm -rf $(DIST_DIR)/$(BINARY_NAME)-$(VERSION)-linux-x86_64

	# Linux ARM64
	@mkdir -p $(DIST_DIR)/$(BINARY_NAME)-$(VERSION)-linux-arm64
	@cp target/aarch64-unknown-linux-musl/release/$(BINARY_NAME) $(DIST_DIR)/$(BINARY_NAME)-$(VERSION)-linux-arm64/
	@cp config.yaml.example $(DIST_DIR)/$(BINARY_NAME)-$(VERSION)-linux-arm64/ 2>/dev/null || true
	@cp routes.yaml.example $(DIST_DIR)/$(BINARY_NAME)-$(VERSION)-linux-arm64/ 2>/dev/null || true
	@cd $(DIST_DIR) && tar czf $(BINARY_NAME)-$(VERSION)-linux-arm64.tar.gz $(BINARY_NAME)-$(VERSION)-linux-arm64/
	@rm -rf $(DIST_DIR)/$(BINARY_NAME)-$(VERSION)-linux-arm64

	# macOS x86_64
	@mkdir -p $(DIST_DIR)/$(BINARY_NAME)-$(VERSION)-macos-x86_64
	@cp target/x86_64-apple-darwin/release/$(BINARY_NAME) $(DIST_DIR)/$(BINARY_NAME)-$(VERSION)-macos-x86_64/
	@cp config.yaml.example $(DIST_DIR)/$(BINARY_NAME)-$(VERSION)-macos-x86_64/ 2>/dev/null || true
	@cp routes.yaml.example $(DIST_DIR)/$(BINARY_NAME)-$(VERSION)-macos-x86_64/ 2>/dev/null || true
	@cd $(DIST_DIR) && tar czf $(BINARY_NAME)-$(VERSION)-macos-x86_64.tar.gz $(BINARY_NAME)-$(VERSION)-macos-x86_64/
	@rm -rf $(DIST_DIR)/$(BINARY_NAME)-$(VERSION)-macos-x86_64

	# macOS ARM64 (Apple Silicon)
	@mkdir -p $(DIST_DIR)/$(BINARY_NAME)-$(VERSION)-macos-arm64
	@cp target/aarch64-apple-darwin/release/$(BINARY_NAME) $(DIST_DIR)/$(BINARY_NAME)-$(VERSION)-macos-arm64/
	@cp config.yaml.example $(DIST_DIR)/$(BINARY_NAME)-$(VERSION)-macos-arm64/ 2>/dev/null || true
	@cp routes.yaml.example $(DIST_DIR)/$(BINARY_NAME)-$(VERSION)-macos-arm64/ 2>/dev/null || true
	@cd $(DIST_DIR) && tar czf $(BINARY_NAME)-$(VERSION)-macos-arm64.tar.gz $(BINARY_NAME)-$(VERSION)-macos-arm64/
	@rm -rf $(DIST_DIR)/$(BINARY_NAME)-$(VERSION)-macos-arm64

	# Windows x86_64
	@mkdir -p $(DIST_DIR)/$(BINARY_NAME)-$(VERSION)-windows-x86_64
	@cp target/x86_64-pc-windows-gnu/release/$(BINARY_NAME).exe $(DIST_DIR)/$(BINARY_NAME)-$(VERSION)-windows-x86_64/
	@cp config.yaml.example $(DIST_DIR)/$(BINARY_NAME)-$(VERSION)-windows-x86_64/ 2>/dev/null || true
	@cp routes.yaml.example $(DIST_DIR)/$(BINARY_NAME)-$(VERSION)-windows-x86_64/ 2>/dev/null || true
	@cd $(DIST_DIR) && zip -q $(BINARY_NAME)-$(VERSION)-windows-x86_64.zip -r $(BINARY_NAME)-$(VERSION)-windows-x86_64/
	@rm -rf $(DIST_DIR)/$(BINARY_NAME)-$(VERSION)-windows-x86_64

	@echo ""
	@echo "✅ Release artifacts in $(DIST_DIR)/:"
	@ls -lh $(DIST_DIR)/

# ─── Clean ────────────────────────────────────────────────────────────────────

clean:
	cargo clean
	rm -rf $(DIST_DIR)

# ─── Test ─────────────────────────────────────────────────────────────────────

test:
	cargo test -- --test-threads=1

test-quick:
	cargo test -- --test-threads=1 --skip proptest
