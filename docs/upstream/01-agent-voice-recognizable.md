# Make agent voices recognizable across huddles

**Size:** one line. **Risk:** none. **File:** `desktop/src-tauri/src/huddle/agent_voice.rs`

## Problem

Auto-assigned agent voices are seeded with `huddle_generation`, so the same agent gets a
different voice in every huddle:

```rust
fn stable_voice_index(agent_pubkey: &str, huddle_generation: u64, len: usize) -> usize {
    let hash = agent_pubkey.bytes().fold(
        0xcbf2_9ce4_8422_2325_u64 ^ huddle_generation,
        |hash, byte| hash.wrapping_mul(0x0000_0100_0000_01b3) ^ u64::from(byte),
    );
    (hash as usize) % len
}
```

The function is named `stable_voice_index`, but it is only stable *within* one huddle.

## Why it matters

In a voice huddle the user often is not looking at the screen. Voice is the only signal of
who is speaking. If an agent sounds different every session, that signal never becomes
learnable, and the per-agent voice feature loses most of its value.

## Change

Drop `huddle_generation` from the seed:

```rust
fn stable_voice_index(agent_pubkey: &str, len: usize) -> usize {
    let hash = agent_pubkey.bytes().fold(
        0xcbf2_9ce4_8422_2325_u64,
        |hash, byte| hash.wrapping_mul(0x0000_0100_0000_01b3) ^ u64::from(byte),
    );
    (hash as usize) % len
}
```

Update the two call sites and the collision-avoidance loop accordingly.

## Trade-off to raise with maintainers

Variety across sessions may have been deliberate. If so, the fix is to seed from a stable
per-user salt instead of the huddle generation — still varied between users, still constant
for one user over time.
