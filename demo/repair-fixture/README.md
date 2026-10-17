# repair-fixture

Demo target for `sentryclaw repair`. Three defects are planted on purpose:

| Function | Defect | Test that catches it |
|---|---|---|
| `sum_to` | off-by-one: `1..n` excludes `n` | `sum_to_is_inclusive` |
| `is_leap_year` | missing the divisible-by-400 rule | `leap_years` |
| `average` | returns `Some(NaN)` for an empty slice | `average_of_empty_is_none` |

`clamp` is correct and acts as a control.
