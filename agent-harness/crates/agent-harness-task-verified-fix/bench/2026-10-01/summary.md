| Model | Accepted | Rejected | Errors | Accept rate | Accept rate (finished runs) | Mean turns | Mean tokens | Retries |
|---|---|---|---|---|---|---|---|---|
| `openai:gpt-5.6` | 15 | 0 | 0 | 100% | 100% | 4.2 | 7228.3 | 1 |
| `gemini:gemini-3.5-flash` | 2 | 0 | 13 | 13% | 100% | 5.0 | 9653.0 | 26 |

| Case | `openai:gpt-5.6` | `gemini:gemini-3.5-flash` |
|---|---|---|
| average_div_zero | 3/3 | 1/3 |
| buffer_off_by_one | 3/3 | 1/3 |
| pitch_overflow | 3/3 | 0/3 |
| ring_index | 3/3 | 0/3 |
| shift_scale | 3/3 | 0/3 |
