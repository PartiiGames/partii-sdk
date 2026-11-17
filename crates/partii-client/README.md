# partii-client

A blocking client for [partii](https://partii.games)'s public API. `OPS` lists every operation (generated from the public OpenAPI spec at https://partii.games/docs/openapi.yaml); `Client::json("gamesGet", &[("game", "tankii")], None)` calls one.

## Feature flags

`partii_client::flags` is the game's side of Partii's feature flags (https://partii.games/docs, FEATURE-FLAGS): the same behaviour as the web SDK's `flags.ts`.

```rust
use partii_client::flags::{Flags, FlagsConfig, FlagValue, Range};

let flags = Flags::new(FlagsConfig {
    base: "https://api.partii.games".into(),
    key: "pk_…".into(),
    game: "tankii".into(),
    defaults: [
        ("net.shell_rows_on_news".to_string(), FlagValue::Bool(false)),
        ("net.shell_rows".to_string(), FlagValue::Int(2)),
        ("ui.theme".to_string(), FlagValue::Str("neon".into())),
    ].into(),
    ranges: [("net.shell_rows".to_string(), Range { min: Some(0), max: Some(4) })].into(),
    channel: Some("nightly".into()),
    platform: Some("windows".into()),
    cache_path: Some(save_dir.join("partii-flags.json")),
    ..Default::default()
});
flags.get_int("net.shell_rows");          // at once: the last answer, else the compiled default
let f = flags.clone();
std::thread::spawn(move || f.refresh());  // at launch, match start, every f.refresh_s()
flags.match_values();                     // the host's match flags, for the handshake
flags.set_player_token(Some(token));      // after sign-in
flags.override_flag("net.shell_rows", FlagValue::Int(3), Some(2.0))?; // an admin, for 2 hours
flags.clear_override("net.shell_rows")?;
flags.admin()?;                           // the overlay (403 `forbidden` for anyone not an admin)
```

- `Flags::new` reads only the cache file; `get*` answer from memory. `refresh()` blocks up to the timeout (2 s), so run it off the main thread; it never panics or errors. Offline: the kept answer, else the compiled defaults.
- `If-None-Match` with the kept ETag; a 304 keeps the answer. The last answer and a random install id (which buckets % rollouts for guests) live in `cache_path`.
- Only values of the compiled default's type are taken; unknown keys are ignored; an int outside the edge's range or the game's own `ranges` is ignored (the default stays).

License: MIT OR Apache-2.0.
