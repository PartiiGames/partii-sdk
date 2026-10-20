# partii-netcode

Reliability by redundancy for games that send only unreliable datagrams: every packet carries a sequence number, an ack and a 32-bit ack bitfield; clients send their last N inputs in every packet; state goes as deltas against the last snapshot the other side acked. `no_std`, and byte-for-byte the same wire format as partii's JS kit.

License: MIT OR Apache-2.0.
