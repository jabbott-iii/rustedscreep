# Tags a release of rustedscreep and pushes the tag, which starts the release
# workflow (.github/workflows/release.yml; docs/RELEASE.md, "Making a
# release"). For maintainers: agents never tag or push here (CLAUDE.md).
#
#     make release VERSION=v0.1.0             # tag HEAD and push the tag
#     make release VERSION=v0.1.0 DRY_RUN=1   # check, and print the rest
#
# The tag ruleset blocks moving or deleting a pushed tag, so it first checks
# what the workflow's gate will: that VERSION is `v` and HEAD's workspace
# version, that HEAD's deploy/ holds a plain build (one with no features), and
# that HEAD is on the remote's main. The workflow's check job must pass too:
# run `npm run check` on HEAD first. A tag deploys nothing to the MMO: the
# GitHub link uploads what a push to main brings (ADR-012).

REMOTE ?= origin

# Runs a command and prints it, or with DRY_RUN only prints it, as written.
run = $(if $(DRY_RUN),echo "would run: $(1)",set -x; $(1))

# Prints the build ID of the deploy/main.js on stdin (the record `npm run
# stage` writes), or fails when it has none or its build has features: the
# workflow's gate runs the same check.
build_id = node -e 'const t = require("fs").readFileSync(0, "utf8"); const m = t.match(/^const BUILD = \((\{.*\})\);$$/m); if (!m) { console.error("error: deploy/main.js has no build record: stage a build with npm run stage"); process.exit(1); } const b = JSON.parse(m[1]); if (b.features.length) { console.error("error: deploy/ holds the feature build " + b.id + ": stage a plain build first"); process.exit(1); } console.log(b.id);'

.PHONY: release
release:
	@test -n '$(VERSION)' || { echo 'usage: make release VERSION=v<version> [REMOTE=origin] [DRY_RUN=1]' >&2; exit 1; }
	@version=$$(git show HEAD:Cargo.toml | sed -n '/^\[workspace\.package\]/,/^\[/s/^version *= *"\([^"]*\)".*/\1/p'); \
	test '$(VERSION)' = "v$$version" || { echo "error: HEAD's workspace version is $$version, so its tag is v$$version, not $(VERSION)" >&2; exit 1; }
	@build=$$(git show HEAD:deploy/main.js | $(build_id)) || exit 1; \
	echo "HEAD's deploy/ holds build $$build"
	@git fetch --quiet '$(REMOTE)' main
	@git merge-base --is-ancestor HEAD FETCH_HEAD || { echo "error: HEAD is not on $(REMOTE)'s main, and a release is made from main" >&2; exit 1; }
	@git ls-remote --exit-code --tags '$(REMOTE)' 'refs/tags/$(VERSION)' >/dev/null; \
	case $$? in \
	  0) echo 'error: $(REMOTE) already has the tag $(VERSION)' >&2; exit 1 ;; \
	  2) ;; \
	  *) exit 1 ;; \
	esac
	@if git rev-parse --quiet --verify 'refs/tags/$(VERSION)' >/dev/null; then \
	  test "$$(git rev-parse 'refs/tags/$(VERSION)^{commit}')" = "$$(git rev-parse HEAD)" || \
	    { echo 'error: $(VERSION) tags another commit here: remove it with git tag -d $(VERSION)' >&2; exit 1; }; \
	  echo '$(VERSION) already tags HEAD here'; \
	else \
	  $(call run,git tag -a '$(VERSION)' -m 'rustedscreep $(VERSION:v%=%)'); \
	fi
	@$(call run,git push '$(REMOTE)' 'refs/tags/$(VERSION)')
