# relay-world: 2026-11-25 04:19 UTC

Edge: https://partii-edge-staging.partii-games.workers.dev · image registry.fly.io/partii-relay-test:probe-5ed9e94 · regions fra, gru, iad, jnb, lhr, nrt, sin, sjc, syd · wall 19.1 min

## Pass / fail

| Case | Result | Notes |
|---|---|---|
| (a) host one region, members everywhere | PASS | pairs 8, auto lowest 8/8, 7/7 turn <= ws |
| (d) host rotated US/EU/Asia | FAIL | pairs 24, auto lowest 23/24, 21/21 turn <= ws; sin->nrt auto settled on turn (77.0 ms) but best is direct (66.3 ms) |
| (b) UDP blocked: turn over TLS 443 | PASS |  |
| (c) failover (app blackhole of WebRTC) | FAIL | fra: switched to turn, not ws; fra: play gap 1338.0 ms > 1000; gru: switched to direct, not ws; gru: play gap 1316.0 ms > 1000; jnb: switched to turn, not ws; jnb: play gap 1921.0 ms > 1000 |
| (c') failover (iptables UDP drop) | FAIL | fra: never promoted back off ws; gru: switched to direct, not ws or turn; gru: play gap 1242.0 ms > 1000; gru: never promoted back off ws; jnb: play gap 1590.0 ms > 1000; jnb: never promoted back off ws |
| zero machines left | PASS | left: [] |

## RTT matrix, host iad (ms, p50 / p95 / p99; loss; open ms)

| member | ws | turn | direct | auto (settled) | edge colo |
|---|---|---|---|---|---|
| fra | 348 / 724 / 1356; 0.5%; 403 | 99 / 197 / 264; 0%; 2212 | 86 / 87 / 87; 0%; 1406 | 88 / 89 / 90; 0%; 865 (direct) | FRA |
| gru | 342 / 575 / 961; 0%; 302 | 129 / 258 / 324; 0%; 1610 | 163 / 163 / 164; 0%; 1609 | 129 / 285 / 351; 0%; 454 (turn) | GRU |
| jnb | 504 / 770 / 876; 0%; 602 | 244 / 456 / 522; 0%; 3314 | 243 / 466 / 553; 0%; 2709 | 240 / 484 / 612; 0%; 1121 (direct) | JNB |
| lhr | 314 / 700 / 1331; 0%; 402 | 89 / 178 / 244; 0%; 1912 | 87 / 87 / 87; 0%; 1306 | 78 / 79 / 81; 0%; 790 (direct) | LHR |
| nrt | 525 / 955 / 1273; 0%; 603 | 169 / 369 / 475; 0%; 2813 | 172 / 173 / 173; 0%; 2414 | 176 / 177 / 837; 0%; 1160 (direct) | NRT |
| sin | 657 / 1045 / 1186; 0%; 806 | 242 / 498 / 552; 0%; 3819 | 235 / 235 / 236; 0%; 3016 | 249 / 255 / 256; 0.3%; 1576 (direct) | SIN |
| sjc | 316 / 642 / 1129; 0%; 402 | 84 / 162 / 250; 0%; 1708 | 66 / 67 / 68; 0%; 13370 | 67 / 67 / 69; 0.1%; 590 (direct) | SJC |
| syd | 474 / 760 / 1345; 0%; 803 | 222 / 429 / 496; 0%; 3715 | 194 / 195 / 238; 0%; 16270 | 197 / 198 / 517; 0%; 1426 (direct) | SYD |

## RTT matrix, host fra (ms, p50 / p95 / p99; loss; open ms)

| member | ws | turn | direct | auto (settled) | edge colo |
|---|---|---|---|---|---|
| gru | 320 / 410 / 464; 0%; 301 | 138 / 139 / 142; 0%; 2512 | 254 / 255 / 255; 0%; 2716 | 142 / 143 / 522; 0%; 735 (turn) | GRU |
| iad | 211 / 438 / 687; 0%; 202 | 90 / 91 / 92; 0%; 1809 | 90 / 91 / 91; 0%; 1406 | 92 / 93 / 297; 0%; 710 (direct) | IAD |
| jnb | 232 / 298 / 344; 0%; 501 | 184 / 186 / 208; 0%; 14550 | 171 / 172 / 172; 0%; 2108 | 188 / 189 / 189; 0%; 808 (turn) | JNB |
| lhr | 51 / 84 / 119; 0%; 201 | 18 / 18 / 19; 0%; 1105 | 14 / 14 / 15; 0%; 805 | 15 / 16 / 16; 0%; 527 (direct) | LHR |
| nrt | 303 / 1098 / 1153; 0%; 804 | 242 / 242 / 250; 0%; 3521 | 249 / 250 / 359; 0%; 16781 | 232 / 233 / 1107; 0%; 1561 (turn) | NRT |
| sin | 327 / 434 / 626; 0%; 602 | 160 / 161 / 162; 0%; 2916 | 153 / 153 / 154; 0.3%; 2418 | 152 / 153 / 647; 0.5%; 1142 (direct) | SIN |
| sjc | 221 / 436 / 594; 0%; 503 | 151 / 152 / 168; 0%; 15980 | 144 / 144 / 145; 0%; 2012 | 144 / 144 / 354; 0%; 1048 (direct) | SJC |
| syd | 370 / 1466 / 1544; 0%; 902 | 253 / 255 / 258; 0%; 4826 | 344 / 345 / 423; 0%; 4517 | 253 / 254 / 740; 0%; 1575 (turn) | SYD |

## RTT matrix, host sin (ms, p50 / p95 / p99; loss; open ms)

| member | ws | turn | direct | auto (settled) | edge colo |
|---|---|---|---|---|---|
| fra | 360 / 536 / 644; 0%; 503 | 172 / 174 / 175; 0%; 2815 | 156 / 156 / 243; 0%; 2514 | 154 / 154 / 412; 0%; 1232 (direct) | FRA |
| gru | 754 / 1227 / 1319; 0%; 702 | 340 / 588 / 878; 0%; 4328 | 352 / 1010 / 1380; 0%; 17390 | 302 / 306 / 979; 0%; 1395 (turn) | GRU |
| iad | 673 / 1100 / 1157; 0%; 603 | 243 / 244 / 245; 0.9%; 3318 | 247 / 253 / 254; 0%; 3115 | 242 / 243 / 1250; 1.9%; 1459 (turn) | IAD |
| jnb | 444 / 648 / 760; 0%; 702 | 299 / 301 / 306; 0%; 3712 | 145 / 146 / 146; 0%; 16067 | 145 / 146 / 745; 0%; 1362 (direct) | JNB |
| lhr | 339 / 522 / 600; 0%; 502 | 159 / 160 / 160; 0%; 2815 | 161 / 161 / 163; 0%; 14984 | 159 / 160 / 426; 0%; 1338 (turn) | LHR |
| nrt | 130 / 235 / 277; 0%; 402 | 77 / 78 / 79; 0%; 2514 | 66 / 67 / 67; 0%; 1508 | 76 / 77 / 78; 0%; 866 (turn) | NRT |
| sjc | 330 / 526 / 681; 0%; 503 | 174 / 176 / 177; 0%; 3014 | 171 / 171 / 172; 0%; 2409 | 173 / 174 / 509; 0%; 1160 (direct) | SJC |
| syd | 254 / 398 / 469; 0%; 502 | 103 / 127 / 170; 0%; 2107 | 192 / 192 / 193; 0%; 2514 | 97 / 98 / 100; 0%; 964 (turn) | SYD |

## (b) UDP blocked: turn over TLS 443 (host iad)

| region | udp_blocked | turn_opened | turn_p50 | turn_tls | turn_open_ms | auto_final | auto_p50 | ws_p50_open_net |
|---|---|---|---|---|---|---|---|---|
| fra | True | True | 92 | True | 1710 | turn | 90 | 348 |
| gru | True | True | 128 | True | 1807 | turn | 128 | 342 |
| jnb | True | True | 235 | True | 3012 | turn | 248 | 504 |
| lhr | True | True | 81 | True | 1610 | turn | 81 | 314 |
| nrt | True | True | 180 | True | 2614 | turn | 156 | 525 |
| sin | True | True | 234 | True | 3516 | turn | 229 | 657 |
| sjc | True | True | 70 | True | 1608 | turn | 71 | 316 |
| syd | True | True | 220 | True | 3315 | turn | 245 | 474 |

## (c) failover (app blackhole of WebRTC) (host iad)

| region | before | switched_to | switch_ms | play_gap_ms | promote_back_ms | promoted_to | rel_missing | rel_out_of_order | rel_max_rtt_ms | errors |
|---|---|---|---|---|---|---|---|---|---|---|
| fra | direct | turn | 934 | 1338 | 413 | direct | 0 | 0 | 1289 | [] |
| gru | turn | direct | 659 | 1316 | 419 | turn | 0 | 0 | 1375 | [] |
| jnb | direct | turn | 755 | 1921 | 525 | direct | 0 | 0 | 2006 | [] |
| lhr | direct | turn | 954 | 1290 | 445 | direct | 0 | 0 | 1170 | [] |
| nrt | direct | turn | 827 | 1390 | 618 | direct | 0 | 0 | 1359 | [] |
| sin | direct | turn | 955 | 1286 | 736 | direct | 0 | 0 | 1386 | [] |
| sjc | direct | ws | 945 | 1473 | 421 | direct | 0 | 0 | 1420 | [] |
| syd | direct | ws | 888 | 1583 | 671 | direct | 0 | 0 | 1611 | [] |

## (c') failover (iptables UDP drop) (host iad)

| region | before | switched_to | switch_ms | play_gap_ms | promote_back_ms | promoted_to | rel_missing | rel_out_of_order | rel_max_rtt_ms | errors |
|---|---|---|---|---|---|---|---|---|---|---|
| fra | direct | turn | 873 | 969 | – | – | 0 | 0 | 922 | [] |
| gru | turn | direct | 780 | 1242 | – | – | 0 | 0 | 1275 | [] |
| jnb | direct | ws | 908 | 1590 | – | – | 0 | 0 | 1574 | [] |
| lhr | turn | ws | 952 | 1383 | – | – | 0 | 0 | 1310 | [] |
| nrt | turn | ws | 987 | 1222 | – | – | 0 | 0 | 1158 | [] |
| sin | turn | direct | 894 | 1344 | – | – | 0 | 0 | 1290 | [] |
| sjc | direct | ws | 869 | 1160 | 24120 | direct | 0 | 0 | 1109 | [] |
| syd | direct | ws | 954 | 2031 | – | – | 0 | 0 | 2051 | [] |

## Cost

Machines: 2.866 machine-hours × $0.004/h ≈ $0.011; egress ≈ 0.062 GB ≈ $0.002; builder ≈ $0.05. Total ≈ $0.06 (estimate).
