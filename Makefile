# Day-to-day targets. CI runs `make check`, `make test`, `make features`,
# and `make doc` on every pull request; `make ci` runs all four locally.
.PHONY: build test lint fmt check features doc ci pre-publish publish clean

# Each feature a host may turn on alone; `make test` covers them together.
FEATURES := syntax-engine syntax-json syntax-markdown syntax-python syntax-rust syntax-toml

# The version in `Cargo.toml`, which `make publish` publishes and tags.
VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)

build:
	cargo build

test:
	cargo test --all-features

lint:
	cargo clippy --all-targets --all-features -- -D warnings

fmt:
	cargo fmt --all

check:
	cargo fmt --all --check
	$(MAKE) lint

# Lints and tests the crate with no features, then with each feature alone.
features:
	cargo clippy --all-targets --no-default-features -- -D warnings
	cargo test --no-default-features
	for feature in $(FEATURES); do \
		cargo clippy --all-targets --no-default-features --features $$feature -- -D warnings \
			&& cargo test --no-default-features --features $$feature \
			|| exit 1; \
	done

doc:
	RUSTDOCFLAGS="-D warnings" cargo doc --no-deps

# Everything CI checks, in one run: a pass here is a pass there.
ci: check test features doc

# Everything short of publishing: refuses uncommitted changes or a version
# already tagged, runs every check, and packages the crate without uploading.
pre-publish:
	@test -z "$$(git status --porcelain)" || { echo "Commit or stash your changes first."; exit 1; }
	@! git rev-parse -q --verify "refs/tags/v$(VERSION)" >/dev/null || { echo "Tag v$(VERSION) exists: bump the version first."; exit 1; }
	$(MAKE) ci
	cargo publish --dry-run

# Publishes the committed crate to crates.io and tags the release. A
# published version can be yanked but never deleted, so it asks first.
publish: pre-publish
	@read -p "Publish cherry $(VERSION) to crates.io? [y/N] " answer && [ "$$answer" = y ]
	cargo publish
	git tag -a "v$(VERSION)" -m "cherry $(VERSION)"
	git push origin "v$(VERSION)"

clean:
	cargo clean
