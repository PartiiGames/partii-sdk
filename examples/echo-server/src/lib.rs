//! A minimal partii-host game server, written with partii-server.
//!
//! Inputs are 8 bytes: x and y as little-endian f32. Every tick it broadcasts
//! all positions as one unreliable datagram: `[count u32]` then
//! `[player u32][x f32][y f32]` per player. A joining player gets a welcome on
//! the reliable (setup) stream.

use partii_server::{export_game, Game, Host, JoinInfo, PlayerId};

#[derive(Default)]
pub struct Echo {
    players: Vec<(PlayerId, f32, f32)>,
}

impl Game for Echo {
    fn init(_seed: u64) -> Self {
        Echo::default()
    }

    fn on_join(&mut self, host: &mut Host, player: PlayerId, info: JoinInfo) {
        let start = (host.random_u32() % 100) as f32;
        self.players.push((player, start, start));
        let _ = host.send_reliable(player, format!("welcome {} (player {})", info.name, player.0).as_bytes());
        host.log(&format!("join {} {}", player.0, info.name));
    }

    fn on_leave(&mut self, _host: &mut Host, player: PlayerId) {
        self.players.retain(|p| p.0 != player);
    }

    fn on_input(&mut self, _host: &mut Host, player: PlayerId, input: &[u8]) {
        let (Some(x), Some(y)) = (input.get(0..4), input.get(4..8)) else { return };
        if let Some(p) = self.players.iter_mut().find(|p| p.0 == player) {
            p.1 = f32::from_le_bytes(x.try_into().unwrap());
            p.2 = f32::from_le_bytes(y.try_into().unwrap());
        }
    }

    fn tick(&mut self, host: &mut Host, _now_ms: f64) {
        let mut out = Vec::with_capacity(4 + 12 * self.players.len());
        out.extend_from_slice(&(self.players.len() as u32).to_le_bytes());
        for (p, x, y) in &self.players {
            out.extend_from_slice(&p.0.to_le_bytes());
            out.extend_from_slice(&x.to_le_bytes());
            out.extend_from_slice(&y.to_le_bytes());
        }
        let _ = host.broadcast(&out);
    }
}

export_game!(Echo);

#[cfg(test)]
mod tests {
    use super::*;
    use partii_server::testing::Sim;

    #[test]
    fn echoes_positions() {
        let mut sim = Sim::<Echo>::new(60, 1);
        let a = sim.join("Ann");
        sim.step();
        let mut input = 3.0f32.to_le_bytes().to_vec();
        input.extend_from_slice(&4.0f32.to_le_bytes());
        sim.input(a, &input);
        sim.step();
        let last = sim.received(a).last().unwrap();
        assert_eq!(&last[..4], &1u32.to_le_bytes());
        assert_eq!(&last[8..16], &input[..]);
        assert!(String::from_utf8_lossy(&sim.received_reliable(a)[0]).starts_with("welcome Ann"));
    }
}
