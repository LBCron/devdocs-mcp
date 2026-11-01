# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/iamwatchdogs/devdocs-mcp/compare/devdocs-store-v0.1.0...devdocs-store-v0.2.0) - 2026-11-01

### Added

- *(store)* publish generations atomically
- *(store)* search FTS5 through a pure query builder
- *(store)* add canonical-body hashing helpers
- *(store)* open the store with pragmas and path checks
- *(store)* add schema 0001 and the migration runner

### Fixed

- *(store)* resolve the database path before the FUSE mount lookup
- *(store)* deref the best mount length under as_ref
- *(store)* borrow the best mount and rename the covering fstype
- *(store)* close the CIFS gap and fail shut on unknown FUSE origins
- *(store)* compare mount points, not fstype names, when shadowing
- *(store)* read the mountinfo prefix miss through is_err
- *(store)* simplify the mountinfo prefix check
- *(store)* withdraw sources that publish a terminal license state
- *(store)* re-check user_version inside the migration transaction
- *(store)* refuse network-backed FUSE mounts on linux
- *(store)* uniform missing-directory error and calmer concurrent open
- *(store)* compile the platform-gated paths on linux and windows
- *(store)* win the private-file creation race and cite the allow sites

### Other

- *(store)* accept the busy loss in the concurrent open
- *(store)* accept the documented WAL loss in the concurrent open
- *(store)* report the losing concurrent open's error
- *(store)* compare the fetched-at suffix without a char literal
- *(store)* backtick the fuse magic in the linux test comment
- *(store)* split the publish generation test into three
- *(testkit)* return typed errors from the shared fixtures
- *(store)* read statfs and geteuid through nix
- sync the lint policy and sharpen the hashing contract
- *(test)* move the store seed fixtures into devdocs-testkit
- *(store)* bundle the symbol pair and carry error context
- *(store)* keep the no-match phrase out of the intra-doc link
