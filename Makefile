prepare: cleanup
	@python ./scripts/prep_skills.py

cleanup:
	[ -d ./tests/skills/ ] && rm -rf ./tests/skills/
