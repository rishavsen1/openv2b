# Month verification results

## one_month

| policy | total $ | energy $ | demand $ | DR penalty $ | DR incentive $ | peak kW | mean net in DR kW | peak red. vs unc. kW | targets met | unserved | missing kWh | banked kWh | clamped kWh | determinism | referee |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| idle | 7532.20 | 5125.50 | 700.20 | 1774.50 | 68.00 | 60.0 | 56.34 | 25.0 | 11/183 | 1 | 5831.00 | 275.00 | 2210.00 | ok | PASS |
| uncontrolled | 9525.65 | 5621.20 | 991.95 | 2980.50 | 68.00 | 85.0 | 64.99 | 0.0 | 182/183 | 1 | 20.00 | 275.00 | 0.00 | ok | PASS |
| edf | 8318.84 | 5620.39 | 991.95 | 1774.50 | 68.00 | 85.0 | 56.34 | 0.0 | 182/183 | 1 | 20.00 | 275.00 | 0.00 | ok | PASS |
| edf-v2b | 4318.88 | 5491.43 | 991.95 | 79.50 | 2244.00 | 96.0 | 44.19 | -11.0 | 182/183 | 1 | 20.00 | 3487.50 | 0.00 | ok | PASS |
| llf | 8318.84 | 5620.39 | 991.95 | 1774.50 | 68.00 | 85.0 | 56.34 | 0.0 | 182/183 | 1 | 20.00 | 275.00 | 0.00 | ok | PASS |
| llf-v2b | 4318.88 | 5491.43 | 991.95 | 79.50 | 2244.00 | 96.0 | 44.19 | -11.0 | 182/183 | 1 | 20.00 | 3487.50 | 0.00 | ok | PASS |

M30 building-load relaxation in DR windows vs idle (56.34 kW baseline): uncontrolled +8.65 kW, edf +0.00 kW, edf-v2b -12.15 kW, llf +0.00 kW, llf-v2b -12.15 kW

## one_month_lossy

| policy | total $ | energy $ | demand $ | DR penalty $ | DR incentive $ | peak kW | mean net in DR kW | peak red. vs unc. kW | targets met | unserved | missing kWh | banked kWh | clamped kWh | determinism | referee |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| idle | 7532.20 | 5125.50 | 700.20 | 1774.50 | 68.00 | 60.0 | 56.34 | 15.0 | 11/183 | 1 | 5831.00 | 275.00 | 2210.00 | ok | PASS |
| uncontrolled | 9459.82 | 5664.30 | 875.25 | 2988.26 | 68.00 | 75.0 | 65.04 | 0.0 | 182/183 | 1 | 20.00 | 275.00 | 0.00 | ok | PASS |
| edf | 8243.60 | 5661.85 | 875.25 | 1774.50 | 68.00 | 75.0 | 56.34 | 0.0 | 182/183 | 1 | 20.00 | 275.00 | 0.00 | ok | PASS |
| edf-v2b | 4244.36 | 5533.61 | 875.25 | 79.50 | 2244.00 | 75.0 | 44.19 | 0.0 | 182/183 | 1 | 20.00 | 3469.47 | 0.00 | ok | PASS |
| llf | 8243.60 | 5661.85 | 875.25 | 1774.50 | 68.00 | 75.0 | 56.34 | 0.0 | 182/183 | 1 | 20.00 | 275.00 | 0.00 | ok | PASS |
| llf-v2b | 4244.36 | 5533.61 | 875.25 | 79.50 | 2244.00 | 75.0 | 44.19 | 0.0 | 182/183 | 1 | 20.00 | 3469.47 | 0.00 | ok | PASS |

M30 building-load relaxation in DR windows vs idle (56.34 kW baseline): uncontrolled +8.70 kW, edf +0.00 kW, edf-v2b -12.15 kW, llf +0.00 kW, llf-v2b -12.15 kW

## one_month_nopersist

| policy | total $ | energy $ | demand $ | DR penalty $ | DR incentive $ | peak kW | mean net in DR kW | peak red. vs unc. kW | targets met | unserved | missing kWh | banked kWh | clamped kWh | determinism | referee |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| idle | 7532.20 | 5125.50 | 700.20 | 1774.50 | 68.00 | 60.0 | 56.34 | 25.0 | 30/183 | 1 | 3720.00 | 1500.00 | 0.00 | ok | PASS |
| uncontrolled | 10464.85 | 5904.90 | 991.95 | 3636.00 | 68.00 | 85.0 | 69.69 | 0.0 | 182/183 | 1 | 20.00 | 1500.00 | 0.00 | ok | PASS |
| edf | 8613.57 | 5897.12 | 991.95 | 1792.50 | 68.00 | 85.0 | 56.47 | 0.0 | 182/183 | 1 | 20.00 | 1500.00 | 0.00 | ok | PASS |
| edf-v2b | 4963.22 | 6117.77 | 991.95 | 97.50 | 2244.00 | 96.0 | 44.32 | -11.0 | 182/183 | 1 | 20.00 | 3426.50 | 0.00 | ok | PASS |
| llf | 8613.57 | 5897.12 | 991.95 | 1792.50 | 68.00 | 85.0 | 56.47 | 0.0 | 182/183 | 1 | 20.00 | 1500.00 | 0.00 | ok | PASS |
| llf-v2b | 4963.22 | 6117.77 | 991.95 | 97.50 | 2244.00 | 96.0 | 44.32 | -11.0 | 182/183 | 1 | 20.00 | 3426.50 | 0.00 | ok | PASS |

M30 building-load relaxation in DR windows vs idle (56.34 kW baseline): uncontrolled +13.34 kW, edf +0.13 kW, edf-v2b -12.02 kW, llf +0.13 kW, llf-v2b -12.02 kW
