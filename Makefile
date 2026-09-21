BIN := $(HOME)/.cargo/bin

.PHONY: build test lint install au install-au capture release release-publish tap sign-check clean

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

# A signed, notarised, universal dist/mat-<version>.tar.gz — what the Homebrew
# formula installs. Builds from the working tree; nothing is tagged or uploaded.
release:
	@scripts/release.sh

# The whole of cutting a release, in the one order that keeps the tag and the
# download honest: notes, version, tag, build, notarise, upload, tap.
release-publish:
	@test -n "$(VERSION)" || { echo "usage: make release-publish VERSION=0.2.0"; exit 1; }
	@scripts/publish-release.sh $(VERSION)

# On its own, so a formula with a wrong checksum or a tap push that failed is
# fixed by running this again. TAP_PRINT=1 shows the formula and pushes nothing.
tap:
	@test -n "$(VERSION)" || { echo "usage: make tap VERSION=0.2.0"; exit 1; }
	@scripts/update-tap.sh $(if $(TAP_PRINT),--print) $(VERSION)

sign-check:
	@security find-identity -v -p codesigning | grep "Developer ID Application" \
		|| echo "no Developer ID Application certificate in the keychain"
	@xcrun notarytool history --keychain-profile $(or $(NOTARY_PROFILE),notarytool) \
		>/dev/null 2>&1 && echo "  notary profile: $(or $(NOTARY_PROFILE),notarytool) ✓" \
		|| echo "  notary profile $(or $(NOTARY_PROFILE),notarytool) is not stored yet — see scripts/release.sh"

clean:
	cargo clean
	rm -rf swift/.build dist
