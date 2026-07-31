# Month verification results

## one_month

| policy | total $ | energy $ | demand $ | DR penalty $ | DR incentive $ | peak kW | mean net in DR kW | peak red. vs unc. kW | targets met | unserved | missing kWh | banked kWh | clamped kWh | determinism | referee |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| idle | 7532.20 | 5125.50 | 700.20 | 1774.50 | 68.00 | 60.0 | 56.34 | 25.0 | 11/183 | 1 | 5831.00 | 275.00 | 2210.00 | ok | PASS |
| uncontrolled | 9525.65 | 5621.20 | 991.95 | 2980.50 | 68.00 | 85.0 | 64.99 | 0.0 | 182/183 | 1 | 20.00 | 275.00 | 0.00 | ok | PASS |
| policy-0 | 8764.41 | 5590.53 | 805.92 | 2435.95 | 68.00 | 69.1 | 61.23 | 15.9 | 152/183 | 1 | 21.14 | 275.00 | 0.00 | ok | PASS |
| policy-1 | 7860.55 | 5461.35 | 700.20 | 1767.00 | 68.00 | 76.0 | 56.23 | 9.0 | 125/183 | 1 | 526.75 | 582.50 | 109.00 | ok | PASS |
| policy-2 | 7899.50 | 5492.80 | 700.20 | 1774.50 | 68.00 | 96.0 | 56.34 | -11.0 | 152/183 | 1 | 490.00 | 3560.84 | 109.00 | ok | PASS |
| edf | 8779.84 | 5591.10 | 820.79 | 2435.95 | 68.00 | 70.3 | 61.23 | 14.7 | 153/183 | 1 | 21.13 | 275.00 | 0.00 | ok | PASS |
| llf | 8853.75 | 5589.46 | 893.14 | 2439.15 | 68.00 | 76.5 | 61.25 | 8.5 | 152/183 | 1 | 22.47 | 275.00 | 0.00 | ok | PASS |

M30 building-load relaxation in DR windows vs idle (56.34 kW baseline): uncontrolled +8.65 kW, policy-0 +4.89 kW, policy-1 -0.12 kW, policy-2 +0.00 kW, edf +4.89 kW, llf +4.91 kW

## one_month_lossy

| policy | total $ | energy $ | demand $ | DR penalty $ | DR incentive $ | peak kW | mean net in DR kW | peak red. vs unc. kW | targets met | unserved | missing kWh | banked kWh | clamped kWh | determinism | referee |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| idle | 7532.20 | 5125.50 | 700.20 | 1774.50 | 68.00 | 60.0 | 56.34 | 15.0 | 11/183 | 1 | 5831.00 | 275.00 | 2210.00 | ok | PASS |
| uncontrolled | 9459.82 | 5664.30 | 875.25 | 2988.26 | 68.00 | 75.0 | 65.04 | 0.0 | 182/183 | 1 | 20.00 | 275.00 | 0.00 | ok | PASS |
| policy-0 | 8900.76 | 5634.91 | 827.75 | 2506.10 | 68.00 | 70.9 | 61.75 | 4.1 | 11/183 | 1 | 28.03 | 275.00 | 0.00 | ok | PASS |
| policy-1 | 7889.70 | 5490.50 | 700.20 | 1767.00 | 68.00 | 75.0 | 56.23 | 0.0 | 122/183 | 1 | 553.95 | 486.12 | 122.92 | ok | PASS |
| policy-2 | 7929.07 | 5522.37 | 700.20 | 1774.50 | 68.00 | 75.0 | 56.34 | 0.0 | 152/183 | 1 | 504.88 | 3556.98 | 122.92 | ok | PASS |
| edf | 8917.99 | 5635.54 | 844.31 | 2506.14 | 68.00 | 72.3 | 61.75 | 2.7 | 11/183 | 1 | 28.13 | 275.00 | 0.00 | ok | PASS |
| llf | 8867.26 | 5634.02 | 794.71 | 2506.53 | 68.00 | 68.1 | 61.75 | 6.9 | 11/183 | 1 | 28.26 | 275.00 | 0.00 | ok | PASS |

M30 building-load relaxation in DR windows vs idle (56.34 kW baseline): uncontrolled +8.70 kW, policy-0 +5.41 kW, policy-1 -0.12 kW, policy-2 +0.00 kW, edf +5.41 kW, llf +5.41 kW

## one_month_nopersist

| policy | total $ | energy $ | demand $ | DR penalty $ | DR incentive $ | peak kW | mean net in DR kW | peak red. vs unc. kW | targets met | unserved | missing kWh | banked kWh | clamped kWh | determinism | referee |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| idle | 7532.20 | 5125.50 | 700.20 | 1774.50 | 68.00 | 60.0 | 56.34 | 25.0 | 30/183 | 1 | 3720.00 | 1500.00 | 0.00 | ok | PASS |
| uncontrolled | 10464.85 | 5904.90 | 991.95 | 3636.00 | 68.00 | 85.0 | 69.69 | 0.0 | 182/183 | 1 | 20.00 | 1500.00 | 0.00 | ok | PASS |
| policy-0 | 9330.77 | 5824.69 | 805.92 | 2768.16 | 68.00 | 69.1 | 63.67 | 15.9 | 152/183 | 1 | 20.10 | 1500.00 | 0.00 | ok | PASS |
| policy-1 | 8060.91 | 5661.71 | 700.20 | 1767.00 | 68.00 | 76.0 | 56.23 | 9.0 | 122/183 | 1 | 312.50 | 1760.75 | 0.00 | ok | PASS |
| policy-2 | 8313.85 | 5907.15 | 700.20 | 1774.50 | 68.00 | 96.0 | 56.34 | -11.0 | 152/183 | 1 | 290.00 | 3281.01 | 0.00 | ok | PASS |
| edf | 9346.20 | 5825.25 | 820.79 | 2768.16 | 68.00 | 70.3 | 63.67 | 14.7 | 153/183 | 1 | 20.10 | 1500.00 | 0.00 | ok | PASS |
| llf | 9416.53 | 5823.23 | 893.14 | 2768.16 | 68.00 | 76.5 | 63.67 | 8.5 | 152/183 | 1 | 21.43 | 1500.00 | 0.00 | ok | PASS |

M30 building-load relaxation in DR windows vs idle (56.34 kW baseline): uncontrolled +13.34 kW, policy-0 +7.33 kW, policy-1 -0.12 kW, policy-2 +0.00 kW, edf +7.33 kW, llf +7.33 kW
