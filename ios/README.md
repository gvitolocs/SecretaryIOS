# ios/

The SwiftUI shell comes next.

It will be a thin client of `secretary-server`'s `/v1` API
(`../docs/server-api.md`), authenticated with a device token
(`Authorization: Bearer sec_…`) kept in the Keychain. Product logic stays in
the Rust crates and in Lucina, not in Swift.
