# rgit vs. git: main 4b31a34 (2026-11-23)

- rgit: git version 2.56.1.rgit.0.1.0 (commit `4b31a3480650`)
- git: git version 2.56.1 (reference build)
- Machine: titan, Intel(R) Xeon(R) CPU E5-2698 v4 @ 2.20GHz, 80 CPUs, 126 GB RAM, kernel 6.12.116+deb13-amd64, CPU governor schedutil
- hyperfine 1.19.0, warm cache, 10 runs after 2 warmups (3 runs without warmup when one validation run takes over 60 s); 1000 files dirty for status-dirty, add-A, commit
- Note: git and gitea ran with the slow threshold 60 s; linux with 30 s (3 runs without warmup above 30 s, 1 run above 300 s) to fit the time budget
- Run 2026-11-23T12:58:23Z – 2026-11-23T18:14:49Z

Times are hyperfine medians (wall clock); CPU is mean user + system time per run; RSS is the peak resident set size of one validation run (`/usr/bin/time -v`). Ratio = rgit / git (below 1 means rgit is faster).

| Scenario | Repo | git | rgit | rgit/git | CPU git | CPU rgit | RSS git | RSS rgit | Runs | Status |
|---|---|--:|--:|--:|--:|--:|--:|--:|--:|---|
| status-clean | git | 10 ms | 69 ms | 7.12× | 19 ms | 69 ms | 6 MB | 35 MB | 10 | ok |
| status-dirty | git | 21 ms | 100 ms | 4.67× | 31 ms | 101 ms | 6 MB | 49 MB | 10 | ok |
| log-oneline | git | 793 ms | 2.26 s | 2.85× | 790 ms | 2.27 s | 67 MB | 90 MB | 10 | ok |
| log-p-200 | git | 105 ms | 246 ms | 2.34× | 105 ms | 245 ms | 41 MB | 82 MB | 10 | ok |
| log-L | git | 1.27 s | 9.66 s | 7.62× | 1.27 s | 9.65 s | 361 MB | 360 MB | 10 | ok |
| rev-list-objects | git | 4.97 s | 24.75 s | 4.97× | 4.99 s | 24.6 s | 402 MB | 429 MB | 10 | ok |
| cat-file-batch-check | git | 729 ms | 2.22 s | 3.04× | 728 ms | 2.21 s | 325 MB | 339 MB | 10 | ok |
| diff-stat | git | 717 ms | 824 ms | 1.15× | 717 ms | 820 ms | 107 MB | 203 MB | 10 | ok |
| blame | git | 1.27 s | 2.24 s | 1.77× | 1.26 s | 2.23 s | 290 MB | 904 MB | 10 | ok |
| grep | git | 100 ms | 82 ms | 0.82× | 508 ms | 91 ms | 20 MB | 11 MB | 10 | ok |
| fetch-noop | git | 25 ms | 1.06 s | 42× | 28 ms | 1.06 s | 36 MB | 10 MB | 10 | ok |
| index-pack | git | 40.12 s | 19.56 s | 0.49× | 139 s | 43.82 s | 678 MB | 939 MB | 10 | ok |
| pack-objects | git | 2.29 s | 13.44 s | 5.86× | 6.02 s | 84.01 s | 377 MB | 2.5 GB | 10 | ok, output differs |
| repack-ad | git | 13.46 s | 42.21 s | 3.14× | 15.07 s | 135 s | 567 MB | 2.7 GB | 10 | ok |
| clone-local | git | 483 ms | 480 ms | 0.99× | 482 ms | 480 ms | 102 MB | 96 MB | 10 | ok |
| checkout-full | git | 432 ms | 423 ms | 0.98× | 433 ms | 424 ms | 93 MB | 107 MB | 10 | ok |
| checkout-switch | git | 546 ms | 1.09 s | 2× | 548 ms | 1.07 s | 86 MB | 111 MB | 10 | ok |
| add-A | git | 96 ms | 212 ms | 2.21× | 107 ms | 212 ms | 16 MB | 23 MB | 10 | ok |
| commit | git | 77 ms | 221 ms | 2.87× | 92 ms | 207 ms | 15 MB | 56 MB | 10 | ok |
| gc | git | 14.71 s | 56.25 s | 3.82× | 16.89 s | 149 s | 568 MB | 2.7 GB | 10 | ok |
| status-clean | gitea | 20 ms | 108 ms | 5.38× | 35 ms | 108 ms | 6 MB | 35 MB | 10 | ok |
| status-dirty | gitea | 30 ms | 135 ms | 4.51× | 44 ms | 136 ms | 7 MB | 47 MB | 10 | ok |
| log-oneline | gitea | 211 ms | 595 ms | 2.82× | 212 ms | 600 ms | 32 MB | 45 MB | 10 | ok |
| log-p-200 | gitea | 673 ms | 804 ms | 1.19× | 672 ms | 797 ms | 112 MB | 151 MB | 10 | ok |
| log-L | gitea | 435 ms | 1.2 s | 2.75× | 434 ms | 1.19 s | 193 MB | 177 MB | 10 | ok |
| rev-list-objects | gitea | 1.09 s | 3.23 s | 2.95× | 1.07 s | 3.22 s | 343 MB | 355 MB | 10 | ok |
| cat-file-batch-check | gitea | 575 ms | 1.76 s | 3.06× | 575 ms | 1.75 s | 302 MB | 314 MB | 10 | ok |
| diff-stat | gitea | 525 ms | 597 ms | 1.14× | 522 ms | 595 ms | 175 MB | 246 MB | 10 | ok |
| blame | gitea | 330 ms | 568 ms | 1.72× | 327 ms | 570 ms | 135 MB | 217 MB | 10 | ok |
| grep | gitea | 114 ms | 106 ms | 0.93× | 535 ms | 111 ms | 10 MB | 11 MB | 10 | ok |
| fetch-noop | gitea | 19 ms | 92 ms | 4.82× | 21 ms | 92 ms | 23 MB | 11 MB | 10 | ok |
| index-pack | gitea | 36.37 s | 17.35 s | 0.48× | 103 s | 34.2 s | 942 MB | 1 GB | 10 | ok |
| pack-objects | gitea | 2.18 s | 19.26 s | 8.84× | 5.25 s | 75.78 s | 419 MB | 2 GB | 10 | ok, output differs |
| repack-ad | gitea | 5.43 s | 47.92 s | 8.82× | 7.85 s | 110 s | 579 MB | 1.9 GB | 10 | ok |
| clone-local | gitea | 449 ms | 505 ms | 1.12× | 448 ms | 498 ms | 67 MB | 69 MB | 10 | ok |
| checkout-full | gitea | 379 ms | 444 ms | 1.17× | 379 ms | 442 ms | 81 MB | 102 MB | 10 | ok |
| checkout-switch | gitea | 488 ms | 1.05 s | 2.15× | 578 ms | 1.04 s | 161 MB | 118 MB | 10 | ok |
| add-A | gitea | 114 ms | 191 ms | 1.67× | 132 ms | 191 ms | 55 MB | 21 MB | 10 | ok |
| commit | gitea | 110 ms | 228 ms | 2.07× | 128 ms | 218 ms | 15 MB | 51 MB | 10 | ok |
| gc | gitea | 6.2 s | 50.76 s | 8.19× | 8.5 s | 113 s | 577 MB | 2.1 GB | 10 | ok |
| status-clean | linux | 142 ms | 1.9 s | 13.37× | 351 ms | 1.9 s | 30 MB | 747 MB | 10 | ok |
| status-dirty | linux | 159 ms | 1.86 s | 11.74× | 367 ms | 1.88 s | 30 MB | 748 MB | 10 | ok |
| log-oneline | linux | 16.77 s | 42.74 s | 2.55× | 16.61 s | 42.95 s | 1.3 GB | 1.7 GB | 3 | ok |
| log-p-200 | linux | 228 ms | 2.41 s | 10.55× | 227 ms | 2.43 s | 286 MB | 1.1 GB | 10 | ok |
| log-L | linux | 16.88 s | 38.17 s | 2.26× | 16.9 s | 37.92 s | 3.3 GB | 2.9 GB | 3 | ok |
| rev-list-objects | linux | 87.56 s | 337 s | 3.85× | 87.44 s | 337 s | 4 GB | 4.3 GB | 1 | ok |
| cat-file-batch-check | linux | 24.46 s | 65.68 s | 2.68× | 24.58 s | 65.72 s | 4 GB | 4.2 GB | 3 | ok |
| diff-stat | linux | 10.75 s | 14.36 s | 1.34× | 10.74 s | 14.33 s | 1.3 GB | 2.9 GB | 10 | ok |
| blame | linux | 2.08 s | 5.04 s | 2.43× | 2.09 s | 5.05 s | 1.4 GB | 2.4 GB | 10 | ok |
| grep | linux | 1.19 s | 1.63 s | 1.37× | 13.43 s | 1.64 s | 311 MB | 55 MB | 10 | ok |
| fetch-noop | linux | 66 ms | 921 ms | 13.86× | 70 ms | 921 ms | 336 MB | 10 MB | 10 | ok |
| index-pack | linux | 706 s | 380 s | 0.54× | 2631 s | 855 s | 5.3 GB | 8.9 GB | 1 | ok |
| pack-objects | linux | 56.75 s | 257 s | 4.53× | 122 s | 1540 s | 4.8 GB | 12.7 GB | 3 | ok, output differs |
| repack-ad | linux | 252 s | 1000 s | 3.97× | 306 s | 3170 s | 6.6 GB | 14.3 GB | 1 | ok |
| clone-local | linux | 12.14 s | 13.11 s | 1.08× | 12.08 s | 13.08 s | 1.1 GB | 1.2 GB | 10 | ok |
| checkout-full | linux | 12.29 s | 14.25 s | 1.16× | 12.26 s | 14.13 s | 1.3 GB | 1.3 GB | 10 | ok |
| checkout-switch | linux | 8.95 s | 17.35 s | 1.94× | 9.06 s | 17.22 s | 1.2 GB | 1.6 GB | 10 | ok |
| add-A | linux | 338 ms | 1.62 s | 4.79× | 535 ms | 1.61 s | 309 MB | 340 MB | 10 | ok |
| commit | linux | 291 ms | 3.13 s | 10.75× | 491 ms | 2.93 s | 343 MB | 1.3 GB | 10 | ok |
| gc | linux | 294 s | 1223 s | 4.16× | 365 s | 3378 s | 6.6 GB | 14.1 GB | 1 | ok |
