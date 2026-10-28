# Testing

The standard way to run tests for Lyquid is `shaker test Cargo.toml`.

For this app, some test cases are integration tests that require loading actual skills.
To prepare the skill catalog for those tests, run `make prepare` before running the tests.

Run the Python unit tests with `make test`. This downloads the skill fixtures via
`make prepare` first if they are not already present.

# Creating a new Commit

When making a new commit, write a concise commit message that describes what the PR does.
The commit message should follow the conventional commit style with scoped prefixes like `feat: ` for new features, `fix: ` for bug fixes, etc.
You may pick the appropriate keyword for the commit.

Avoid using `git add -A` and always manually select files that should be added to the commit.
