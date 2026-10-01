/* Four-slot ring buffer of air-data samples. */
int ring[4];

/* Store `value` at slot `head`; return the next slot. */
int ring_put(int head, int value) {
  ring[head] = value;
  return (head + 1) % 4;
}
