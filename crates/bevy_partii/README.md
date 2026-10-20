# bevy_partii

[Bevy](https://bevy.org) plugin for [partii](https://partii.games): rooms and join codes, peer-to-peer messages, partii-host match seats, player sign-in, ownership and achievements, as Bevy resources and messages.

```rust
App::new()
    .add_plugins(PartiiPlugin::new("https://api.partii.games", "pk_...", "mygame").net_version("1"))
    .add_systems(Update, |mut ev: MessageReader<RoomEvent>| for e in ev.read() { println!("{e:?}") })
    .run();
```

| bevy_partii | Bevy |
|---|---|
| 0.1 | 0.20 |

License: MIT OR Apache-2.0.
