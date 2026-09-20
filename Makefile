BIN := $(HOME)/.cargo/bin

.PHONY: build test lint install au install-au capture clean

build:
	cargo build --release

test:
	cargo test --workspace

lint:
	cargo clippy --workspace --release

# `mat` into ~/.cargo/bin. It finds the sample library in this checkout.
install:
	cargo install --path crates/mat-cli

# The Audio Unit host (macOS). `mat` looks for it beside itself.
au:
	swift build -c release --package-path swift --product mat-au

install-au: au
	install -m 755 swift/.build/release/mat-au $(BIN)/mat-au

# MatCapture.app, which records other apps (macOS 14.2+).
capture:
	./swift/bundle-capture.sh

clean:
	cargo clean
	rm -rf swift/.build
