# Day-to-day targets. CI runs `make check`, `make test`, `make features`,
# and `make doc` on every pull request and when started by hand; `make ci`
# runs all four locally.
.PHONY: build test lint fmt check features doc ci gui gui-check misty facade publish-checks pre-publish publish clean

# Each feature a host may turn on alone; `make test` covers them together.
FEATURES := serde syntax-engine syntax-json syntax-markdown syntax-python syntax-rust syntax-textmate syntax-toml

# The engine's version, which `make publish` publishes and tags.
VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' crates/cherry-engine/Cargo.toml | head -n 1)

build:
	cargo build -p cherry-engine

test:
	cargo test -p cherry-engine --all-features

lint:
	cargo clippy -p cherry-engine --all-targets --all-features -- -D warnings

fmt:
	cargo fmt --all

check:
	cargo fmt --all --check
	$(MAKE) lint

# Lints and tests the crate with no features, then with each feature alone.
features:
	cargo clippy -p cherry-engine --all-targets --no-default-features -- -D warnings
	cargo test -p cherry-engine --no-default-features
	for feature in $(FEATURES); do \
		cargo clippy -p cherry-engine --all-targets --no-default-features --features $$feature -- -D warnings \
			&& cargo test -p cherry-engine --no-default-features --features $$feature \
			|| exit 1; \
	done

# The GUI example, a crate of its own on egui: `make gui FILE=crates/cherry-engine/src/lib.rs`.
gui:
	cargo run -p cherry-egui -- $(FILE)

gui-check:
	cargo clippy -p cherry-egui -- -D warnings

# Cherry on misty, a crate of its own (`cherry-misty`): linted, tested,
# and formatted apart, so the engine's targets never compile misty.
misty:
	cargo fmt -p cherry-misty --check
	cargo clippy -p cherry-misty --all-targets --all-features -- -D warnings
	cargo test -p cherry-misty --all-features

# The crate apps add (`cherry`), which only re-exports: built with no
# toolkit, as an app on another toolkit takes it, and with every feature,
# its docs too.
facade:
	cargo clippy -p cherry --no-default-features -- -D warnings
	cargo clippy -p cherry --all-features -- -D warnings
	RUSTDOCFLAGS="-D warnings" cargo doc -p cherry --no-deps --all-features

doc:
	RUSTDOCFLAGS="-D warnings" cargo doc -p cherry-engine --no-deps

# Everything CI checks, in one run: a pass here is a pass there.
ci: check test features doc

# What both `pre-publish` and `publish` check first: no uncommitted changes,
# a version not tagged yet, and everything CI checks.
publish-checks:
	@test -z "$$(git status --porcelain)" || { echo "Commit or stash your changes first."; exit 1; }
	@! git rev-parse -q --verify "refs/tags/cherry-engine-v$(VERSION)" >/dev/null || { echo "Tag cherry-engine-v$(VERSION) exists: bump the version first."; exit 1; }
	$(MAKE) ci

# Everything short of publishing: the checks, then the engine packaged and
# built as crates.io would, without uploading.
pre-publish: publish-checks
	cargo publish -p cherry-engine --dry-run
	@echo "Dry run passed: cherry-engine $(VERSION) is ready to publish; nothing was uploaded."

# Publishes the committed engine to crates.io and tags the release, after
# the checks and a yes. `cargo publish` packages and verifies the crate
# itself before uploading. A published version can be yanked but never
# deleted.
publish: publish-checks
	@read -p "Publish cherry-engine $(VERSION) to crates.io? [y/N] " answer && [ "$$answer" = y ]
	cargo publish -p cherry-engine
	git tag -a "cherry-engine-v$(VERSION)" -m "cherry-engine $(VERSION)"
	git push origin "cherry-engine-v$(VERSION)"

clean:
	cargo clean
