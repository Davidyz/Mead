# Housekeeping

When creating commits, use the conventional commit style where every commit message starts with a keyword like `fix:`, `feat:`, `refactor:`, followed by a concise descriptive message.
You should use `git log` to read recent commit messages to learn the pattern.

# Project Management

This repository is hosted on GitHub.
Use the `gh` CLI or the repo URL `https://github.com/Davidyz/Mead` to read the issues and/or pull requests when needed.
A high-level project design is available in issue 1 (`https://github.com/Davidyz/Mead/issues/1`).

# External Resourses

- See the Lyquor documentation at `https://docs.rs/lyquid/latest/lyquid/index.html`.
- The application will be deployed with the shaker CLI. See its documentation at `https://docs.rs/shaker/latest/shaker/`.
- The documentation for the LDE (Lyquid Development Kit) is also available at `https://docs.lyquor.dev/docs/ldk/`.

# Coding Guidelines

- Prefer Lyquid prelude types over stdlib equivalents. Use `lyquid::prelude::{HashMap, HashSet, new_hashmap(), new_hashset()}` instead of `std::collections::{HashMap, HashSet}`. The Lyquid versions use deterministically seeded hashers required for WASM execution and on-chain consistency.
- Always initialize Lyquid collections with `new_hashmap()` / `new_hashset()` rather than `Default::default()` or `.new()`.
