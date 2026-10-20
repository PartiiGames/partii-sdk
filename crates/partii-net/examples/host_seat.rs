//! Join a match on a partii-host and print what comes back.
//!
//! Local development (a host started without ticket keys takes the join in the
//! form "<match>\n<player>\n<info JSON>", with no session key):
//!
//! ```sh
//! cargo run -p partii-net --example host_seat -- 127.0.0.1:4435 my-match
//! ```
//!
//! It sends an 8-byte input (x, y as f32) every 50 ms for two seconds.

use partii_net::host::{join, SeatEvent, SeatOptions};
use partii_net::protocol::host::Endpoint;
use std::time::Duration;

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let udp = args.next().unwrap_or_else(|| "127.0.0.1:4435".into());
    let match_id = args.next().unwrap_or_else(|| "my-match".into());
    let ep = Endpoint {
        udp,
        ticket: format!("{match_id}\nexample.1\n{{\"name\":\"Example\"}}"),
        key: String::new(),
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let seat = join(&ep, &SeatOptions::default(), move |e| {
        let _ = tx.send(e);
    })?;
    let mut states = 0;
    for i in 0..40 {
        let mut input = (i as f32).to_le_bytes().to_vec();
        input.extend_from_slice(&2.0f32.to_le_bytes());
        seat.input(&input)?;
        std::thread::sleep(Duration::from_millis(50));
        while let Ok(e) = rx.try_recv() {
            match e {
                SeatEvent::Frame(f) => {
                    states += 1;
                    if states <= 3 || states % 20 == 0 {
                        println!("frame type {:#04x}, {} bytes", f[0], f.len());
                    }
                }
                SeatEvent::Closed(why) => return Err(why),
            }
        }
    }
    println!("{states} frames in 2 s");
    Ok(())
}
