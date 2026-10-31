CRATE_VER := $(shell cargo info --color never lyquid | grep -E '^version:' | sed 's/version: //')
SHAKER_VER := $(shell shaker -V | sed 's/shaker //')
CRATE_NORM := $(shell echo "$(CRATE_VER)" | sed 's/+.*//')
SHAKER_NORM := $(shell echo $(SHAKER_VER) | sed 's/+.*//')


.PHONY: prepare check-ldk test cleanup

prepare: cleanup
	uv sync --group dev
	@python ./scripts/prep_skills.py

cleanup:
	[ ! -d ./tests/skills/ ] || rm -rf ./tests/skills/

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
	@$(MAKE) prepare
