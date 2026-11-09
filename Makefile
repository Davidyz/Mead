CRATE_VER := $(shell cargo info --color never lyquid | grep -E '^version:' | sed 's/version: //')
SHAKER_VER := $(shell shaker -V | sed 's/shaker //')
CRATE_NORM := $(shell echo "$(CRATE_VER)" | sed 's/+.*//')
SHAKER_NORM := $(shell echo $(SHAKER_VER) | sed 's/+.*//')

# Optional existing Lyquid ID for `make prepare-nodes`.
LYQUID_ID ?=
# Comma-separated node indexes that should not receive skills.
SKIP_UPLOAD_NODES ?=
PREP_NODES_ARGS := $(if $(LYQUID_ID),--lyquid-id $(LYQUID_ID),) \
	$(if $(SKIP_UPLOAD_NODES),--skip-nodes $(SKIP_UPLOAD_NODES),)


.PHONY: prepare-skills prepare-nodes check-ldk test cleanup

prepare-skills: cleanup
	uv sync --group dev
	@python ./scripts/prep_skills.py

# Prepare per-node fixtures, upload skills, activate the search oracle, and
# self-register every node. This requires the stock multi-node Docker devnet
# from the LDK to be running.
prepare-nodes: tests/skills/skills
	@echo "This requires the stock multi-node Docker devnet from the LDK to be running."
	uv run python ./scripts/prep_nodes.py $(PREP_NODES_ARGS)

cleanup:
	[ ! -d ./tests/skills/ ] || rm -rf ./tests/skills/
	[ ! -d ./tests/fixture/ ] || rm -rf ./tests/fixture/

check-ldk:
	@echo "lyquid crate version: $(CRATE_NORM)"
	@echo "shaker version: $(SHAKER_NORM)"
	@if [ "$(CRATE_NORM)" = "$(SHAKER_NORM)" ]; then \
		echo "✓ Versions match."; \
	else \
		echo "✗ Versions DO NOT match!"; exit 1; \
	fi

test: test-python

test-python: tests/skills/skills
	uv run pytest tests

tests/skills/skills:
	@$(MAKE) prepare-skills
