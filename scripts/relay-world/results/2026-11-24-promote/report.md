# relay-world: 2026-11-25 05:43 UTC

Edge: https://partii-edge-staging.partii-games.workers.dev · image registry.fly.io/partii-relay-test:probe-90f48e1 · regions fra, gru, iad, jnb, lhr, nrt, sin, sjc, syd · wall 4.6 min

## Pass / fail

| Case | Result | Notes |
|---|---|---|
| (a) host one region, members everywhere | FAIL | pairs 0, auto lowest 0/0, 0/0 turn <= ws |
| (d) host rotated US/EU/Asia | FAIL | pairs 0, auto lowest 0/0, 0/0 turn <= ws |
| (c) failover (app blackhole of WebRTC) | PASS |  |
| (c') failover (iptables UDP drop) | FAIL | fra: never promoted back off ws; gru: never promoted back off ws; jnb: switched to None, not ws or turn; jnb: never promoted back off ws; lhr: never promoted back off ws; nrt: never promoted back off ws |
| zero machines left | PASS | left: [] |

## (c) failover (app blackhole of WebRTC) (host iad)

| region | before | switched_to | switch_ms | play_gap_ms | promote_back_ms | promoted_to | rel_missing | rel_out_of_order | rel_max_rtt_ms | errors |
|---|---|---|---|---|---|---|---|---|---|---|
| fra | direct | ws | 884 | 452 | 433 | direct | 0 | 0 | 817 | [] |
| gru | turn | ws | 840 | 556 | 611 | turn | 0 | 0 | 978 | [] |
| jnb | direct | ws | 912 | 649 | 711 | direct | 0 | 0 | 1114 | [] |
| lhr | direct | ws | 852 | 465 | 363 | direct | 0 | 0 | 769 | [] |
| nrt | turn | ws | 779 | 558 | 555 | turn | 0 | 0 | 978 | [] |
| sin | direct | ws | 995 | 690 | 513 | direct | 0 | 0 | 1567 | [] |
| sjc | direct | ws | 910 | 422 | 456 | direct | 0 | 0 | 939 | [] |
| syd | direct | ws | 918 | 964 | 603 | direct | 0 | 0 | 1392 | [] |

## (c') failover (iptables UDP drop) (host iad)

| region | before | switched_to | switch_ms | play_gap_ms | promote_back_ms | promoted_to | rel_missing | rel_out_of_order | rel_max_rtt_ms | errors |
|---|---|---|---|---|---|---|---|---|---|---|
| fra | turn | ws | 974 | 522 | – | – | 0 | 0 | 929 | [] |
| gru | turn | ws | 773 | 425 | – | – | 0 | 0 | 566 | [] |
| jnb | turn | – | – | 43 | – | – | 0 | 0 | 656 | [] |
| lhr | direct | ws | 959 | 504 | – | – | 0 | 0 | 761 | [] |
| nrt | turn | ws | 872 | 460 | – | – | 0 | 0 | 935 | [] |
| sin | turn | ws | 871 | 572 | – | – | 0 | 0 | 1113 | [] |
| sjc | direct | ws | 899 | 470 | 6481 | direct | 0 | 0 | 586 | [] |
| syd | direct | ws | 775 | 528 | 7464 | direct | 0 | 0 | 954 | [] |

## Cost

Machines: 0.689 machine-hours × $0.004/h ≈ $0.003; egress ≈ 0.017 GB ≈ $0.001; builder ≈ $0.05. Total ≈ $0.05 (estimate).
