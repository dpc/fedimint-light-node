# Development

This project uses the Linked Specs convention; consult the `linked-specs`
skill before working with specs or governed code.
Read `SECURITY.md` before changing networking, persistence, or trust boundaries.

Load dpc's Rust project, code-style, crate-preference, and Jujutsu workflow
skills before implementation. Run `nix build -L .#ci.tests` and
`nix develop -c selfci check` before completing a change.
When unsure about project or code practices, use `~/lab/tau/tau` as a
read-only reference.

The node is not implemented yet. Preserve the distinction between intended
contracts and implemented behavior in status sections when extending it.
