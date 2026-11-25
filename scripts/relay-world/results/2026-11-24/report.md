# relay-world: 2026-11-25 04:43 UTC

Edge: https://partii-edge-staging.partii-games.workers.dev · image registry.fly.io/partii-relay-test:probe-8b99898 · regions fra, gru, iad, jnb, lhr, nrt, sin, sjc, syd · wall 19.3 min

## Pass / fail

| Case | Result | Notes |
|---|---|---|
| (a) host one region, members everywhere | PASS | pairs 8, auto lowest 8/8, 7/7 turn <= ws |
| (d) host rotated US/EU/Asia | PASS | pairs 24, auto lowest 24/24, 21/21 turn <= ws |
| (b) UDP blocked: turn over TLS 443 | FAIL | syd: never joined the room (['join: internal: Error: Network connection lost.', 'not in the room within 20 s']): an edge error, not a path |
| (c) failover (app blackhole of WebRTC) | PASS |  |
| (c') failover (iptables UDP drop) | PASS |  |
| zero machines left | PASS | left: [] |

## RTT matrix, host iad (ms, p50 / p95 / p99; loss; open ms)

| member | ws | turn | direct | auto (settled) | edge colo |
|---|---|---|---|---|---|
| fra | 132 / 373 / 683; 0.1%; 403 | 92 / 93 / 93; 0%; 1712 | 87 / 87 / 87; 0%; 1709 | 90 / 91 / 283; 0%; 749 (direct) | FRA |
| gru | 158 / 208 / 242; 0%; 403 | 135 / 136 / 136; 0%; 10670 | 133 / 133 / 134; 0%; 2012 | 131 / 131 / 132; 0%; 579 (direct) | GRU |
| jnb | 393 / 538 / 621; 0%; 604 | 248 / 249 / 250; 0%; 2912 | 235 / 235 / 236; 0%; 11758 | 243 / 244 / 461; 0%; 1029 (direct) | JNB |
| lhr | 144 / 276 / 330; 0%; 403 | 79 / 80 / 82; 0%; 1708 | 74 / 75 / 75; 0%; 1503 | 74 / 75 / 76; 0%; 620 (direct) | LHR |
| nrt | 470 / 763 / 810; 0%; 504 | 193 / 195 / 202; 0%; 2612 | 186 / 403 / 438; 0%; 11473 | 183 / 398 / 710; 0%; 1233 (turn) | NRT |
| sin | 324 / 1074 / 1130; 0%; 704 | 229 / 230 / 231; 0%; 3518 | 249 / 250 / 252; 0%; 4628 | 254 / 257 / 835; 0.1%; 1453 (direct) | SIN |
| sjc | 130 / 244 / 289; 0%; 302 | 76 / 78 / 199; 0%; 1608 | 91 / 92 / 92; 0%; 10766 | 64 / 65 / 65; 0%; 638 (direct) | SJC |
| syd | 281 / 1053 / 1114; 0%; 703 | 208 / 209 / 210; 0%; 4422 | 198 / 198 / 198; 0%; 3614 | 199 / 200 / 1027; 0%; 1482 (direct) | SYD |

## RTT matrix, host fra (ms, p50 / p95 / p99; loss; open ms)

| member | ws | turn | direct | auto (settled) | edge colo |
|---|---|---|---|---|---|
| gru | 415 / 572 / 648; 0%; 504 | 143 / 144 / 144; 0%; 2213 | 202 / 203 / 203; 0%; 2414 | 141 / 142 / 306; 0%; 807 (turn) | GRU |
| iad | 222 / 318 / 384; 0%; 302 | 90 / 91 / 98; 0%; 1608 | 90 / 91 / 91; 0%; 1605 | 93 / 94 / 234; 0%; 779 (direct) | IAD |
| jnb | 192 / 236 / 286; 0%; 502 | 179 / 180 / 180; 0%; 2308 | 162 / 162 / 163; 0%; 2110 | 163 / 164 / 167; 0%; 749 (direct) | JNB |
| lhr | 31 / 50 / 77; 0%; 101 | 16 / 16 / 17; 0%; 9744 | 15 / 16 / 16; 0%; 602 | 18 / 18 / 19; 0%; 359 (turn) | LHR |
| nrt | 657 / 1067 / 1132; 0%; 704 | 254 / 255 / 255; 0%; 3519 | 250 / 252 / 252; 0.4%; 3320 | 231 / 232 / 677; 0%; 1481 (turn) | NRT |
| sin | 337 / 513 / 650; 0%; 604 | 156 / 158 / 524; 0%; 2714 | 155 / 156 / 156; 0.3%; 2514 | 155 / 155 / 309; 0.1%; 1227 (direct) | SIN |
| sjc | 310 / 534 / 639; 0%; 302 | 151 / 153 / 166; 0.4%; 2512 | 163 / 164 / 164; 0%; 2213 | 158 / 158 / 404; 0%; 1009 (direct) | SJC |
| syd | 893 / 1447 / 1521; 0%; 804 | 254 / 256 / 258; 0%; 3816 | 252 / 253 / 254; 0%; 3616 | 250 / 251 / 926; 0%; 1877 (direct) | SYD |

## RTT matrix, host sin (ms, p50 / p95 / p99; loss; open ms)

| member | ws | turn | direct | auto (settled) | edge colo |
|---|---|---|---|---|---|
| fra | 330 / 479 / 586; 0%; 603 | 172 / 172 / 173; 0%; 2817 | 154 / 155 / 339; 0%; 2414 | 154 / 155 / 206; 0%; 1171 (direct) | FRA |
| gru | 483 / 995 / 1195; 0%; 805 | 306 / 308 / 309; 0%; 3823 | 349 / 350 / 421; 0%; 4122 | 300 / 302 / 566; 0%; 1658 (turn) | GRU |
| iad | 640 / 805 / 899; 0%; 604 | 233 / 240 / 246; 0%; 3314 | 244 / 244 / 245; 0%; 3311 | 241 / 242 / 726; 0%; 1532 (direct) | IAD |
| jnb | 462 / 617 / 722; 0%; 702 | 298 / 300 / 302; 0%; 3916 | 145 / 146 / 147; 0%; 2812 | 145 / 146 / 683; 0%; 1288 (direct) | JNB |
| lhr | 348 / 497 / 630; 0%; 503 | 165 / 166 / 167; 0%; 2713 | 224 / 224 / 225; 0%; 3115 | 157 / 158 / 412; 0%; 1180 (turn) | LHR |
| nrt | 154 / 254 / 335; 0%; 302 | 76 / 78 / 80; 0%; 1811 | 68 / 68 / 69; 0%; 1508 | 68 / 68 / 69; 0%; 725 (direct) | NRT |
| sjc | 324 / 470 / 597; 0%; 302 | 168 / 170 / 463; 0%; 2514 | 164 / 165 / 165; 0%; 2714 | 164 / 165 / 649; 0%; 886 (direct) | SJC |
| syd | 191 / 345 / 402; 0%; 502 | 99 / 102 / 103; 0%; 10556 | 202 / 203 / 203; 0%; 2511 | 98 / 99 / 246; 0%; 898 (turn) | SYD |

## (b) UDP blocked: turn over TLS 443 (host iad)

| region | udp_blocked | turn_opened | turn_p50 | turn_tls | turn_open_ms | auto_final | auto_p50 | ws_p50_open_net |
|---|---|---|---|---|---|---|---|---|
| fra | True | True | 100 | True | 3119 | turn | 104 | 132 |
| gru | True | True | 141 | True | 2614 | turn | 135 | 158 |
| jnb | True | True | 258 | True | 3512 | turn | 254 | 393 |
| lhr | True | True | 90 | True | 2215 | turn | 88 | 144 |
| nrt | True | True | 175 | True | 3122 | turn | 170 | 470 |
| sin | True | True | 235 | True | 4524 | turn | 242 | 324 |
| sjc | True | True | 78 | True | 2010 | turn | 88 | 130 |
| syd | True | False | – | False | – | turn | 234 | 281 |

## (c) failover (app blackhole of WebRTC) (host iad)

| region | before | switched_to | switch_ms | play_gap_ms | promote_back_ms | promoted_to | rel_missing | rel_out_of_order | rel_max_rtt_ms | errors |
|---|---|---|---|---|---|---|---|---|---|---|
| fra | turn | ws | 947 | 479 | 394 | direct | 0 | 0 | 956 | [] |
| gru | turn | ws | 964 | 553 | 395 | direct | 0 | 0 | 1004 | [] |
| jnb | turn | ws | 840 | 679 | 611 | turn | 0 | 0 | 1149 | [] |
| lhr | direct | ws | 954 | 526 | 405 | direct | 0 | 0 | 954 | [] |
| nrt | direct | ws | 814 | 585 | 594 | direct | 0 | 0 | 1053 | [] |
| sin | direct | ws | 811 | 671 | 541 | turn | 0 | 0 | 1304 | [] |
| sjc | turn | ws | 859 | 429 | 348 | turn | 0 | 0 | 1021 | [] |
| syd | direct | ws | 910 | 627 | 678 | direct | 0 | 0 | 1229 | [] |

## (c') failover (iptables UDP drop) (host iad)

| region | before | switched_to | switch_ms | play_gap_ms | promote_back_ms | promoted_to | rel_missing | rel_out_of_order | rel_max_rtt_ms | errors |
|---|---|---|---|---|---|---|---|---|---|---|
| fra | direct | ws | 924 | 722 | 6795 | direct | 0 | 0 | 1166 | [] |
| gru | direct | ws | 811 | 542 | 17411 | direct | 0 | 0 | 1092 | [] |
| jnb | direct | ws | 878 | 663 | 8666 | direct | 0 | 0 | 1198 | [] |
| lhr | direct | ws | 850 | 545 | 6678 | direct | 0 | 0 | 1161 | [] |
| nrt | turn | ws | 768 | 582 | 28728 | direct | 0 | 0 | 1201 | [] |
| sin | turn | ws | 772 | 687 | 10159 | direct | 0 | 0 | 1597 | [] |
| sjc | direct | ws | 792 | 697 | 7268 | direct | 0 | 0 | 1179 | [] |
| syd | direct | ws | 897 | 637 | 8014 | direct | 0 | 0 | 1147 | [] |

## Cost

Machines: 2.896 machine-hours × $0.004/h ≈ $0.012; egress ≈ 0.066 GB ≈ $0.003; builder ≈ $0.05. Total ≈ $0.06 (estimate).
