# Development

- Read README.md and follow parent workspace rules.
- Use official Rust mcap through a stable C ABI and .NET SafeHandle. Never expose Rust layout or panic across the ABI.
- Version 0.1.0; first native target win-x64. Keep Cargo.lock and all dependencies pinned.
- No dependencies on LibRobot or business payloads. No commits, pushes or publishing without authorization.
- Worker owns this repository including its build scripts, tests and docs. Coordinate any external changes with the root agent.
